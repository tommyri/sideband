use serde::Deserialize;
use tauri::menu::AboutMetadata;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BuildIdentity {
    schema_version: u32,
    channel: String,
    version: String,
    commit: String,
    dirty: bool,
    target: String,
    origin: String,
    profile: String,
}

pub(crate) fn about() -> AboutMetadata<'static> {
    // Generated and validated by build.rs from our packaging script or dev defaults.
    let build: BuildIdentity = serde_json::from_str(include_str!(concat!(
        env!("OUT_DIR"),
        "/build-identity.json"
    )))
    .expect("build.rs generated valid build identity");
    let details = format!(
        "Channel: {}\nCommit: {}\nSource: {} ({})\nTarget: {}\nProfile: {}\nReport schema: {}\n\nTesting build with voice UI and guided evidence export. Cross-platform acceptance remains open.",
        build.channel,
        build.commit,
        if build.dirty {
            "modified working tree"
        } else {
            "clean checkout"
        },
        build.origin,
        build.target,
        build.profile,
        build.schema_version,
    );
    AboutMetadata {
        name: Some("Sideband — test build".to_owned()),
        version: Some(build.version),
        short_version: Some(build.channel),
        comments: Some(details.clone()),
        credits: Some(details),
        ..AboutMetadata::default()
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn embedded_identity_is_visible_in_platform_about_fields() {
        let about = super::about();
        let details = about.comments.unwrap();
        assert_eq!(about.credits.unwrap(), details);
        assert!(details.contains("Commit:"));
        assert!(details.contains("Target:"));
        assert!(details.contains("voice UI"));
        assert!(about.version.is_some());
    }
}
