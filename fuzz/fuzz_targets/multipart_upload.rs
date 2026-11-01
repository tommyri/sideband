#![no_main]

//! The `POST /api/v1/attachments` multipart reader and file-name cleanup.
//!
//! The first byte picks one of a few boundaries (including awkward ones), the
//! rest is the request body. Accepted forms must respect the size cap.

use std::sync::OnceLock;

use libfuzzer_sys::fuzz_target;
use ss_server::fuzzing::{stored_filename, upload_form};

const MAX_BYTES: u64 = 4096;
const BOUNDARIES: [&str; 4] = ["ssbound", "x", "----WebKitFormBoundary7MA4YWxkTrZu0gW", "a\"b"];

fn runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("current-thread runtime")
    })
}

fuzz_target!(|data: &[u8]| {
    let Some((&selector, body)) = data.split_first() else {
        return;
    };
    let boundary = BOUNDARIES[usize::from(selector) % BOUNDARIES.len()];
    let content_type = if selector & 0x80 == 0 {
        format!("multipart/form-data; boundary={boundary}")
    } else {
        format!("multipart/form-data; boundary=\"{boundary}\"")
    };
    let result = runtime().block_on(upload_form(&content_type, body.to_vec(), MAX_BYTES));
    if let Ok(form) = result {
        assert!(!form.bytes.is_empty());
        assert!(u64::try_from(form.bytes.len()).unwrap() <= MAX_BYTES);
        assert!(form.channel_id.len() <= 64);
        let stored = stored_filename(&form.filename);
        assert!(!stored.is_empty());
        assert!(stored.chars().count() <= 200);
        assert!(!stored.contains(['/', '\\']), "{stored:?}");
        assert!(!stored.chars().any(char::is_control), "{stored:?}");
        assert!(stored != "." && stored != "..");
    }
});
