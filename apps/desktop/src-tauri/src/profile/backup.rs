use super::{
    Vault,
    catalog::{self, PortableProfile},
    install, locked, record, storage_error, vault,
};
use crate::{
    state::{AppState, lock},
    types::ApiError,
};
use serde::{Deserialize, Serialize};
use ss_client_core::{profile::ProfileCredentials, v1};
use ss_core::Id;
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Mutex,
};
use tauri::State;
use zeroize::Zeroizing;
mod format;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Grant {
    source: Vec<u8>,
    authorization: Vec<u8>,
}

impl Grant {
    pub(super) fn to_wire(&self) -> v1::RecoveryGrant {
        v1::RecoveryGrant {
            source: self.source.clone().into(),
            authorization: self.authorization.clone().into(),
        }
    }
}
pub(super) type Grants = BTreeMap<String, Grant>;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Body {
    profile: PortableProfile,
    grants: Grants,
}

impl Body {
    fn validate(&self) -> Result<(), ApiError> {
        self.profile.validate().map_err(|_| damaged())?;
        if self.profile.memberships.len() != self.grants.len() {
            return Err(damaged());
        }
        for member in &self.profile.memberships {
            let id = member.server_id().map_err(|_| damaged())?;
            let grant = self.grants.get(id.as_str()).ok_or_else(damaged)?;
            if grant.source.len() != 32 || grant.authorization.len() != 64 {
                return Err(damaged());
            }
        }
        Ok(())
    }
}

fn damaged() -> ApiError {
    ApiError::new(
        "backup_damaged",
        "This backup file is incomplete or damaged. Choose another copy from your drive, USB drive or password manager.",
    )
}
fn wrong_key() -> ApiError {
    ApiError::new(
        "backup_key",
        "These words didn’t open this file. Check all 12 words and their order, or choose the file saved with this key.",
    )
}
pub(super) fn already_exists() -> ApiError {
    ApiError::new(
        "profile_exists",
        "This computer already has a profile. Restore on a fresh installation to keep that profile safe.",
    )
}
fn file_error(_: impl std::fmt::Display) -> ApiError {
    ApiError::new(
        "backup_file",
        "Sideband couldn’t open or save that file. Choose another location or check your access.",
    )
}

fn portable(path: &Path, vault: &impl Vault) -> Result<(Zeroizing<[u8; 32]>, Body), ApiError> {
    let record = record(path)?.ok_or_else(catalog::missing)?;
    let roots = vault.read(&record.key_id)?;
    let (root, device) = roots.split_first_chunk::<32>().ok_or_else(locked)?;
    let device = device.try_into().map_err(|_| locked())?;
    let mut grants = BTreeMap::new();
    for member in &record.memberships {
        let server = member.server_id()?;
        let user = member.user_id.parse().map_err(storage_error)?;
        let grant = if let Some(pending) = record.recovery_grants.get(server.as_str()) {
            // The new device may not have reached this server yet. Keep the
            // prior source until the server confirms this device's enrollment.
            pending.clone()
        } else {
            let keys = ProfileCredentials::new(root, device, &server, String::new(), String::new())
                .map_err(crate::commands::api_error)?;
            let proof = keys.recovery_grant(user);
            Grant {
                source: proof.source.to_vec(),
                authorization: proof.authorization.to_vec(),
            }
        };
        grants.insert(server.as_str().to_owned(), grant);
    }
    let body = Body {
        profile: PortableProfile {
            name: record.name,
            memberships: record.memberships,
        },
        grants,
    };
    body.validate()?;
    Ok((Zeroizing::new(*root), body))
}

fn export(path: &Path, vault: &impl Vault) -> Result<(Vec<u8>, String), ApiError> {
    let (root, body) = portable(path, vault)?;
    format::seal(root.as_ref(), &body)
}

pub(super) fn transfer(path: &Path, vault: &impl Vault) -> Result<Zeroizing<Vec<u8>>, ApiError> {
    let (root, body) = portable(path, vault)?;
    let mut bytes = Zeroizing::new(root.to_vec());
    serde_json::to_writer(&mut *bytes, &body).map_err(storage_error)?;
    Ok(bytes)
}

fn restore(path: &Path, file: &[u8], words: &str, vault: &impl Vault) -> Result<String, ApiError> {
    if record(path)?.is_some() {
        return Err(already_exists());
    }
    let (root, body) = format::open(file, words)?;
    install_portable(path, &root, body, vault)
}

pub(super) fn install_transfer(
    path: &Path,
    bytes: &[u8],
    vault: &impl Vault,
) -> Result<String, ApiError> {
    let (root, json) = bytes.split_first_chunk::<32>().ok_or_else(damaged)?;
    let body: Body = serde_json::from_slice(json).map_err(|_| damaged())?;
    body.validate()?;
    install_portable(path, root, body, vault)
}

fn install_portable(
    path: &Path,
    root: &[u8; 32],
    body: Body,
    vault: &impl Vault,
) -> Result<String, ApiError> {
    let mut roots = Zeroizing::new([0u8; 64]);
    roots[..32].copy_from_slice(root);
    getrandom::fill(&mut roots[32..]).map_err(storage_error)?;
    install(
        path,
        body.profile.name,
        body.profile.memberships,
        body.grants,
        &roots,
        vault,
    )
}

fn save_new(path: &Path, bytes: &[u8]) -> Result<(), ApiError> {
    let mut file =
        tempfile::NamedTempFile::new_in(path.parent().ok_or_else(|| file_error("path"))?)
            .map_err(file_error)?;
    file.write_all(bytes).map_err(file_error)?;
    file.as_file().sync_all().map_err(file_error)?;
    file.persist_noclobber(path).map_err(|e| if e.error.kind() == std::io::ErrorKind::AlreadyExists {
        ApiError::new("backup_exists","There is already a file with that name. Choose a new name to keep your earlier backup and its words.")
    } else { file_error(e) })?;
    Ok(())
}

#[derive(Default)]
pub(crate) struct PendingBackup(Mutex<Option<(Id, Vec<u8>)>>);

#[derive(Serialize)]
pub(crate) struct Selected {
    id: Id,
    filename: String,
}

#[derive(Serialize)]
pub(crate) struct Saved {
    filename: String,
    words: String,
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
async fn pick(window: &tauri::Window, save: bool) -> Result<Option<PathBuf>, ApiError> {
    if window.label() != "main" {
        return Err(file_error("window"));
    }
    let dialog = rfd::AsyncFileDialog::new()
        .set_parent(window)
        .add_filter("Sideband profile backup", &["sideband-backup"]);
    let file = if save {
        dialog
            .set_file_name("My profile.sideband-backup")
            .save_file()
            .await
    } else {
        dialog.pick_file().await
    };
    Ok(file.map(|f| f.path().to_owned()))
}
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
async fn pick(_: &tauri::Window, _: bool) -> Result<Option<PathBuf>, ApiError> {
    Err(ApiError::new(
        "profile_unavailable",
        "Profile backups are available on Mac and Windows.",
    ))
}

#[tauri::command]
pub(crate) async fn save_profile_backup(
    window: tauri::Window,
    state: State<'_, AppState>,
) -> Result<Option<Saved>, ApiError> {
    let Some(file) = pick(&window, true).await? else {
        return Ok(None);
    };
    let _operation = state.operations.lock().await;
    let path = state.profile_path.clone();
    tokio::task::spawn_blocking(move || {
        let (bytes, words) = export(&path, &vault::System)?;
        save_new(&file, &bytes)?;
        Ok(Some(Saved {
            filename: file
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            words,
        }))
    })
    .await
    .map_err(storage_error)?
}

#[tauri::command]
pub(crate) async fn choose_profile_backup(
    window: tauri::Window,
    pending: State<'_, PendingBackup>,
) -> Result<Option<Selected>, ApiError> {
    let Some(path) = pick(&window, false).await? else {
        return Ok(None);
    };
    let (filename, bytes) = tokio::task::spawn_blocking(move || {
        let mut bytes = Vec::new();
        fs::File::open(&path)
            .map_err(file_error)?
            .take(format::MAX_FILE as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(file_error)?;
        format::inspect(&bytes)?;
        Ok::<_, ApiError>((
            path.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            bytes,
        ))
    })
    .await
    .map_err(storage_error)??;
    let id = Id::generate();
    *lock(&pending.0) = Some((id, bytes));
    Ok(Some(Selected { id, filename }))
}

#[tauri::command]
pub(crate) async fn restore_profile_backup(
    window: tauri::Window,
    state: State<'_, AppState>,
    pending: State<'_, PendingBackup>,
    id: Id,
    words: String,
) -> Result<String, ApiError> {
    let words = Zeroizing::new(words);
    if window.label() != "main" {
        return Err(file_error("window"));
    }
    let _operation = state.operations.lock().await;
    let bytes = lock(&pending.0)
        .as_ref()
        .filter(|(selected, _)| *selected == id)
        .map(|(_, bytes)| bytes.clone())
        .ok_or_else(|| {
            ApiError::new("backup_missing", "Choose the backup file you saved first.")
        })?;
    let path = state.profile_path.clone();
    let name = tokio::task::spawn_blocking(move || restore(&path, &bytes, &words, &vault::System))
        .await
        .map_err(storage_error)??;
    let mut slot = lock(&pending.0);
    if slot.as_ref().is_some_and(|(selected, _)| *selected == id) {
        *slot = None;
    }
    Ok(name)
}

#[cfg(test)]
mod tests;
