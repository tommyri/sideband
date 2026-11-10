//! Checks on the shell's static configuration: the webview capability and the
//! test-build config overlay.
use serde_json::Value;
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

/// Permissions each Tauri JS API import needs. App commands registered with
/// `generate_handler!` need none. Rust-side plugin use (`StoreExt`,
/// window-state restore, notifications, opener, updater) needs none either.
const API_PERMISSIONS: &[(&str, &str, &[&str])] = &[
    ("@tauri-apps/api/core", "invoke", &[]),
    (
        "@tauri-apps/api/event",
        "listen",
        &["core:event:allow-listen", "core:event:allow-unlisten"],
    ),
];

fn manifest(path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(path)
}

fn json(path: &str) -> Value {
    serde_json::from_str(&std::fs::read_to_string(manifest(path)).unwrap()).unwrap()
}

fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if path.is_dir() {
            if name != "generated" {
                sources(&path, out);
            }
        } else if path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("ts") || e.eq_ignore_ascii_case("tsx"))
            && !name.contains(".test.")
        {
            out.push(path);
        }
    }
}

/// `(module, name)` for every value imported from a `@tauri-apps/*` module.
fn tauri_imports(source: &str) -> Vec<(String, String)> {
    let mut found = Vec::new();
    for (at, _) in source.match_indices("from '@tauri-apps/") {
        let module_start = at + "from '".len();
        let module_end = module_start + source[module_start..].find('\'').unwrap();
        let module = &source[module_start..module_end];
        let statement = &source[source[..at].rfind("import").unwrap()..at];
        let names = statement
            .split_once('{')
            .and_then(|(_, rest)| rest.split_once('}'))
            .map_or_else(
                || statement.trim_start_matches("import"),
                |(names, _)| names,
            );
        for name in names.split(',').map(str::trim) {
            if !name.is_empty() && !name.starts_with("type ") {
                found.push((module.to_owned(), name.to_owned()));
            }
        }
    }
    found
}

#[test]
fn capability_grants_only_what_the_frontend_imports() {
    let mut files = Vec::new();
    sources(&manifest("../src"), &mut files);
    assert!(!files.is_empty(), "frontend sources not found");
    let mut needed = BTreeSet::new();
    for file in &files {
        for (module, name) in tauri_imports(&std::fs::read_to_string(file).unwrap()) {
            let Some((.., permissions)) = API_PERMISSIONS
                .iter()
                .find(|(m, n, _)| *m == module && *n == name)
            else {
                panic!(
                    "{} imports `{name}` from {module}; add its permissions to \
                     API_PERMISSIONS and capabilities/default.json",
                    file.display()
                );
            };
            needed.extend(permissions.iter().map(|p| (*p).to_owned()));
        }
    }
    let capability = json("capabilities/default.json");
    assert_eq!(capability["windows"], serde_json::json!(["main"]));
    let granted: BTreeSet<String> = capability["permissions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p.as_str().unwrap().to_owned())
        .collect();
    assert_eq!(granted, needed, "capabilities/default.json");
}

#[test]
fn import_scanner_reads_named_and_multiline_imports() {
    let source = "import { listen, type UnlistenFn } from '@tauri-apps/api/event'\n\
                  import {\n  invoke,\n} from '@tauri-apps/api/core'\n\
                  import { load } from '@tauri-apps/plugin-store'";
    assert_eq!(
        tauri_imports(source),
        [
            ("@tauri-apps/api/event".into(), "listen".into()),
            ("@tauri-apps/api/core".into(), "invoke".into()),
            ("@tauri-apps/plugin-store".into(), "load".into()),
        ]
    );
}

/// Tauri merges `--config` files as a JSON merge patch, so the overlay's
/// `windows` array replaces the base array instead of merging into it.
#[test]
fn test_build_windows_keep_every_base_window_setting() {
    let base = json("tauri.conf.json");
    let overlay = json("tauri.test.conf.json");
    let base = base["app"]["windows"].as_array().unwrap();
    let Some(test) = overlay["app"]["windows"].as_array() else {
        return;
    };
    assert_eq!(base.len(), test.len());
    for (base, test) in base.iter().zip(test) {
        for (key, value) in base.as_object().unwrap() {
            if key != "title" {
                assert_eq!(&test[key], value, "test window `{key}`");
            }
        }
    }
}
