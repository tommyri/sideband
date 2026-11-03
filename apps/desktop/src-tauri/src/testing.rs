//! Local, explicit export only. No app stores, raw logs or microphone data are read.
use crate::types::ApiError;
use serde::Deserialize;
use std::{
    fs::OpenOptions,
    io::Write,
    time::{SystemTime, UNIX_EPOCH},
};
use tauri::Manager;

#[tauri::command]
pub fn test_build_identity() -> serde_json::Value {
    serde_json::from_str(include_str!(concat!(
        env!("OUT_DIR"),
        "/build-identity.json"
    )))
    .expect("validated build identity")
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportFile {
    name: String,
    bytes: Vec<u8>,
}

fn error(message: impl ToString) -> ApiError {
    ApiError::new("test_export", message.to_string())
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0_u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & 0_u32.wrapping_sub(crc & 1));
        }
    }
    !crc
}
fn word(out: &mut Vec<u8>, n: u16) {
    out.extend(n.to_le_bytes());
}
fn long(out: &mut Vec<u8>, n: u32) {
    out.extend(n.to_le_bytes());
}
/// ZIP STORE (no compression). Inputs are bounded before conversion to ZIP32 lengths.
fn archive(
    files: &[ExportFile],
    consent: bool,
    include_recording: bool,
) -> Result<Vec<u8>, ApiError> {
    let mut names = std::collections::HashSet::new();
    let total: usize = files.iter().map(|f| f.bytes.len()).sum();
    if files.len() > 5 || total > 16 * 1024 * 1024 {
        return Err(error("Export exceeds 16 MiB limit"));
    }
    for file in files {
        if !matches!(
            file.name.as_str(),
            "report.json" | "schema.json" | "diagnostics.log" | "notes.txt" | "recording.wav"
        ) || !names.insert(&file.name)
        {
            return Err(error("Invalid export filename"));
        }
        if file.name == "recording.wav"
            && (!consent
                || !include_recording
                || !file.bytes.starts_with(b"RIFF")
                || file.bytes.get(8..12) != Some(b"WAVE"))
        {
            return Err(error(
                "WAV export requires participant consent and separate export choice",
            ));
        }
    }
    let report = files
        .iter()
        .find(|f| f.name == "report.json")
        .ok_or_else(|| error("Missing report"))?;
    let json: serde_json::Value = serde_json::from_slice(&report.bytes).map_err(error)?;
    if json["schemaVersion"] != 1 || !json["runId"].is_string() {
        return Err(error("Unsupported report schema"));
    }
    let mut out = Vec::new();
    let mut directory = Vec::new();
    for file in files {
        let offset = u32::try_from(out.len()).map_err(error)?;
        let length = u32::try_from(file.bytes.len()).map_err(error)?;
        let name_length = u16::try_from(file.name.len()).map_err(error)?;
        let crc = crc32(&file.bytes);
        long(&mut out, 0x0403_4b50);
        word(&mut out, 20);
        word(&mut out, 0);
        word(&mut out, 0);
        word(&mut out, 0);
        word(&mut out, 33);
        long(&mut out, crc);
        long(&mut out, length);
        long(&mut out, length);
        word(&mut out, name_length);
        word(&mut out, 0);
        out.extend(file.name.as_bytes());
        out.extend(&file.bytes);
        long(&mut directory, 0x0201_4b50);
        word(&mut directory, 20);
        word(&mut directory, 20);
        word(&mut directory, 0);
        word(&mut directory, 0);
        word(&mut directory, 0);
        word(&mut directory, 33);
        long(&mut directory, crc);
        long(&mut directory, length);
        long(&mut directory, length);
        word(&mut directory, name_length);
        word(&mut directory, 0);
        word(&mut directory, 0);
        word(&mut directory, 0);
        word(&mut directory, 0);
        long(&mut directory, 0);
        long(&mut directory, offset);
        directory.extend(file.name.as_bytes());
    }
    let start = u32::try_from(out.len()).map_err(error)?;
    let size = u32::try_from(directory.len()).map_err(error)?;
    out.extend(directory);
    long(&mut out, 0x0605_4b50);
    word(&mut out, 0);
    word(&mut out, 0);
    let count = u16::try_from(files.len()).map_err(error)?;
    word(&mut out, count);
    word(&mut out, count);
    long(&mut out, size);
    long(&mut out, start);
    word(&mut out, 0);
    Ok(out)
}
#[tauri::command]
pub async fn export_test_report(
    app: tauri::AppHandle,
    files: Vec<ExportFile>,
    consent: bool,
    include_recording: bool,
) -> Result<String, ApiError> {
    tauri::async_runtime::spawn_blocking(move || {
        let bytes = archive(&files, consent, include_recording)?;
        let folder = app
            .path()
            .download_dir()
            .map_err(error)?
            .join("SocialSpeak reports");
        std::fs::create_dir_all(&folder).map_err(error)?;
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(error)?
            .as_nanos();
        let path = folder.join(format!("socialspeak-report-{stamp}.zip"));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(error)?;
        file.write_all(&bytes).map_err(error)?;
        Ok(path.to_string_lossy().into_owned())
    })
    .await
    .map_err(error)?
}
#[cfg(test)]
mod tests {
    use super::*;
    fn report() -> ExportFile {
        ExportFile {
            name: "report.json".into(),
            bytes: br#"{"schemaVersion":1,"runId":"test"}"#.to_vec(),
        }
    }
    #[test]
    fn zip_has_valid_crc_and_directory() {
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
        let zip = archive(&[report()], false, false).unwrap();
        assert_eq!(&zip[..4], b"PK\x03\x04");
        assert_eq!(&zip[zip.len() - 22..zip.len() - 18], b"PK\x05\x06");
        let count = u16::from_le_bytes(zip[zip.len() - 12..zip.len() - 10].try_into().unwrap());
        assert_eq!(count, 1);
    }
    #[test]
    fn rejects_paths_future_schema_and_unconsented_recordings() {
        let mut file = report();
        file.name = "../secret".into();
        assert!(archive(&[file], false, false).is_err());
        let mut file = report();
        file.bytes = br#"{"schemaVersion":2,"runId":"test"}"#.to_vec();
        assert!(archive(&[file], false, false).is_err());
        for (consent, export) in [(false, false), (true, false), (false, true)] {
            let wav = ExportFile {
                name: "recording.wav".into(),
                bytes: b"RIFF0000WAVE".to_vec(),
            };
            assert!(archive(&[report(), wav], consent, export).is_err());
        }
        assert!(
            archive(
                &[
                    report(),
                    ExportFile {
                        name: "recording.wav".into(),
                        bytes: b"RIFF0000WAVE".to_vec()
                    }
                ],
                true,
                true
            )
            .is_ok()
        );
    }
}
