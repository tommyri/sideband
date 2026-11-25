//! Membership metadata lives in the app directory. The long-lived signing
//! secrets stay in the operating system's credential store.

use crate::{state::AppState, types::ApiError};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use ss_client_core::{Credentials, profile::ProfileCredentials};
use ss_net::pins::ServerId;
use std::{
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
};
use tauri::State;
use zeroize::Zeroizing;

pub(crate) mod backup;
pub(crate) mod catalog;
mod vault;

#[derive(Serialize, Deserialize)]
struct Record {
    name: String,
    key_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    memberships: Vec<catalog::Membership>,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    recovery_grants: backup::Grants,
}

trait Vault {
    fn put(&self, id: &str, secret: &[u8]) -> Result<(), ApiError>;
    fn read(&self, id: &str) -> Result<Zeroizing<Vec<u8>>, ApiError>;
    fn remove(&self, id: &str) -> Result<(), ApiError>;
}

fn locked() -> ApiError {
    ApiError::new(
        "profile_locked",
        "Could not open your profile keys. Check that your system credential store is unlocked.",
    )
}

fn storage_error(_: impl std::fmt::Display) -> ApiError {
    ApiError::new(
        "profile_storage",
        "Could not read or save the local profile.",
    )
}

fn record(path: &Path) -> Result<Option<Record>, ApiError> {
    match fs::File::open(path) {
        Ok(file) => serde_json::from_reader(file)
            .map(Some)
            .map_err(storage_error),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(storage_error(error)),
    }
}

fn existing(record: Record, requested: &str) -> Result<String, ApiError> {
    if record.name == requested {
        Ok(record.name)
    } else {
        Err(ApiError::new(
            "profile_exists",
            "This installation already has a profile.",
        ))
    }
}

fn create(path: &Path, name: &str, vault: &impl Vault) -> Result<String, ApiError> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 64 || name.chars().any(char::is_control) {
        return Err(ApiError::new(
            "invalid_name",
            "Enter a profile name between 1 and 64 characters.",
        ));
    }
    if let Some(current) = record(path)? {
        return existing(current, name);
    }
    let mut roots = Zeroizing::new([0_u8; 64]);
    getrandom::fill(roots.as_mut()).map_err(storage_error)?;
    match install(
        path,
        name.to_owned(),
        Vec::new(),
        backup::Grants::new(),
        &roots,
        vault,
    ) {
        Err(error) if error.code == "profile_exists" => {
            existing(record(path)?.ok_or_else(catalog::missing)?, name)
        }
        result => result,
    }
}

// Publish the descriptor last. A failed import cannot replace the current profile
// or partially publish its restored memberships, pins or credential-store item.
fn install(
    path: &Path,
    name: String,
    memberships: Vec<catalog::Membership>,
    recovery_grants: backup::Grants,
    roots: &[u8; 64],
    vault: &impl Vault,
) -> Result<String, ApiError> {
    let parent = path
        .parent()
        .ok_or_else(|| storage_error("missing directory"))?;
    fs::create_dir_all(parent).map_err(storage_error)?;
    let mut file = tempfile::NamedTempFile::new_in(parent).map_err(storage_error)?;
    let mut id = [0; 16];
    getrandom::fill(&mut id).map_err(storage_error)?;
    let profile = Record {
        name,
        key_id: URL_SAFE_NO_PAD.encode(id),
        memberships,
        recovery_grants,
    };
    serde_json::to_writer(file.as_file_mut(), &profile).map_err(storage_error)?;
    file.as_file().sync_all().map_err(storage_error)?;
    vault.put(&profile.key_id, roots)?;
    match file.persist_noclobber(path) {
        Ok(_) => Ok(profile.name),
        Err(error) => {
            vault.remove(&profile.key_id)?;
            if error.error.kind() == ErrorKind::AlreadyExists {
                return Err(backup::already_exists());
            }
            Err(storage_error(error))
        }
    }
}

fn profile_credentials(
    path: &Path,
    server: &ServerId,
    username: String,
    invite: String,
    vault: &impl Vault,
) -> Result<Credentials, ApiError> {
    let record = record(path)?
        .ok_or_else(|| ApiError::new("profile_missing", "Create a local profile first."))?;
    let roots = vault.read(&record.key_id)?;
    let (membership, device) = roots.split_first_chunk::<32>().ok_or_else(locked)?;
    let device = device.try_into().map_err(|_| locked())?;
    let mut scoped = ProfileCredentials::new(membership, device, server, username, invite)
        .map_err(crate::commands::api_error)?;
    if let Some(member) = record
        .memberships
        .iter()
        .find(|m| m.server_id().ok().as_ref() == Some(server))
    {
        let user = member.user_id.parse().map_err(storage_error)?;
        scoped = if let Some(grants) = record.recovery_grants.get(server.as_str()) {
            scoped.recovering(user, grants.to_wire())
        } else {
            scoped.existing_member(user)
        };
    }
    Ok(Credentials::Profile(Box::new(scoped)))
}

/// Read only the recoverable half. The device root remains installation-local.
pub(crate) fn membership_root(path: &Path) -> Result<Zeroizing<[u8; 32]>, ApiError> {
    let profile = record(path)?.ok_or_else(catalog::missing)?;
    let roots = vault::System.read(&profile.key_id)?;
    let roots: &[u8; 64] = roots.as_slice().try_into().map_err(|_| locked())?;
    Ok(Zeroizing::new(
        roots[..32].try_into().map_err(|_| locked())?,
    ))
}

#[tauri::command]
pub async fn get_local_profile(state: State<'_, AppState>) -> Result<Option<String>, ApiError> {
    vault::available()?;
    let path = state.profile_path.clone();
    tokio::task::spawn_blocking(move || record(&path).map(|r| r.map(|p| p.name)))
        .await
        .map_err(storage_error)?
}

#[tauri::command]
pub async fn create_local_profile(
    state: State<'_, AppState>,
    name: String,
) -> Result<String, ApiError> {
    vault::available()?;
    let _operation = state.operations.lock().await;
    let path = state.profile_path.clone();
    tokio::task::spawn_blocking(move || create(&path, &name, &vault::System))
        .await
        .map_err(storage_error)?
}

pub async fn credentials(
    path: PathBuf,
    server: ServerId,
    username: String,
    invite: String,
) -> Result<Credentials, ApiError> {
    vault::available()?;
    tokio::task::spawn_blocking(move || {
        profile_credentials(&path, &server, username, invite, &vault::System)
    })
    .await
    .map_err(storage_error)?
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, collections::HashMap};
    #[derive(Default)]
    pub(super) struct MemoryVault(RefCell<HashMap<String, Zeroizing<Vec<u8>>>>);

    impl Vault for MemoryVault {
        fn put(&self, id: &str, secret: &[u8]) -> Result<(), ApiError> {
            self.0
                .borrow_mut()
                .insert(id.to_owned(), Zeroizing::new(secret.to_owned()));
            Ok(())
        }
        fn read(&self, id: &str) -> Result<Zeroizing<Vec<u8>>, ApiError> {
            self.0.borrow().get(id).cloned().ok_or_else(locked)
        }
        fn remove(&self, id: &str) -> Result<(), ApiError> {
            self.0.borrow_mut().remove(id);
            Ok(())
        }
    }

    #[test]
    fn creating_and_reopening_a_profile_preserves_its_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profile.json");
        let vault = MemoryVault::default();
        assert!(record(&path).unwrap().is_none());
        assert_eq!(create(&path, "   Alice   ", &vault).unwrap(), "Alice");
        let saved: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        let item = saved["key_id"].as_str().unwrap();
        assert_eq!(saved, serde_json::json!({"name":"Alice","key_id":item}));
        let roots = vault.read(item).unwrap();
        assert_eq!(roots.len(), 64);
        assert_ne!(roots[..32], roots[32..]);
        create(&path, "Alice", &vault).unwrap();
        assert_eq!(
            create(&path, "Bob", &vault).unwrap_err().code,
            "profile_exists"
        );
        assert_eq!(vault.0.borrow().len(), 1);
        assert_eq!(vault.read(item).unwrap(), roots);
        let credentials = || {
            profile_credentials(
                &path,
                &ServerId::new("example.org", 443).unwrap(),
                "alice".into(),
                String::new(),
                &vault,
            )
        };
        assert!(credentials().is_ok());
        vault.remove(item).unwrap();
        assert_eq!(credentials().err().unwrap().code, "profile_locked");
        assert!(vault.0.borrow().is_empty());
    }
}
