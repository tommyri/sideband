//! The portable subset of a profile. Only record an account after the server
//! has authenticated this profile or acknowledged linking the existing account.
use super::{Record, record, storage_error};
use crate::{persistence::SavedServer, state::AppState, types::ApiError};
use serde::{Deserialize, Serialize};
use ss_client_core::Client;
use ss_net::pins::{PinStore, ServerId, TrustPolicy};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Membership {
    pub host: String,
    pub port: u16,
    pub https_port: u16,
    pub name: String,
    pub username: String,
    pub user_id: String,
    pub trust: TrustPolicy,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct PortableProfile {
    pub name: String,
    pub memberships: Vec<Membership>,
}

pub(super) fn missing() -> ApiError {
    ApiError::new("profile_missing", "Create a local profile first.")
}

fn invalid() -> ApiError {
    ApiError::new(
        "invalid_profile",
        "The profile contains invalid membership details.",
    )
}

fn valid_name(value: &str, limit: usize) -> bool {
    !value.trim().is_empty()
        && value.chars().count() <= limit
        && !value.chars().any(char::is_control)
}

impl Membership {
    pub(crate) fn server_id(&self) -> Result<ServerId, ApiError> {
        ServerId::new(&self.host, self.port).map_err(|_| invalid())
    }
    fn validate(&self) -> Result<ServerId, ApiError> {
        if self.port == 0
            || self.https_port == 0
            || self.host.len() > 255
            || !valid_name(&self.name, 256)
            || !valid_name(&self.username, 32)
            || self.user_id.parse::<ss_core::Id>().is_err()
        {
            return Err(invalid());
        }
        self.server_id()
    }
}

impl PortableProfile {
    pub(crate) fn validate(&self) -> Result<(), ApiError> {
        if !valid_name(&self.name, 64) || self.memberships.len() > 4096 {
            return Err(invalid());
        }
        let mut seen = BTreeSet::new();
        for membership in &self.memberships {
            if !seen.insert(membership.validate()?) {
                return Err(invalid());
            }
        }
        Ok(())
    }
}

pub(crate) fn snapshot(path: &Path) -> Result<PortableProfile, ApiError> {
    let current = record(path)?.ok_or_else(missing)?;
    let result = PortableProfile {
        name: current.name,
        memberships: current.memberships,
    };
    result.validate()?;
    Ok(result)
}

pub(crate) fn memberships(path: &Path) -> Result<Vec<Membership>, ApiError> {
    Ok(record(path)?.map(|p| p.memberships).unwrap_or_default())
}

/// Names and addresses from a restored profile are available immediately,
/// even before a new session can be opened with their server.
pub(crate) fn server_list(
    mut saved: Vec<SavedServer>,
    portable: Vec<Membership>,
) -> Result<Vec<(SavedServer, Option<String>)>, ApiError> {
    let known: BTreeSet<_> = saved.iter().map(|s| s.id.clone()).collect();
    let mut usernames = BTreeMap::new();
    for member in portable {
        let id = member.server_id()?.as_str().to_owned();
        usernames.insert(id.clone(), member.username);
        if !known.contains(&id) {
            saved.push(SavedServer {
                id,
                host: member.host,
                port: member.port,
                https_port: member.https_port,
                label: member.name,
            });
        }
    }
    Ok(saved
        .into_iter()
        .map(|s| {
            let name = usernames.remove(&s.id);
            (s, name)
        })
        .collect())
}

/// Restore pins lazily. A trust decision already made on this installation
/// takes precedence over a backup, including a different pin for this host.
pub(crate) fn seed_trust(path: &Path, pins: &dyn PinStore, id: &ServerId) -> Result<(), ApiError> {
    let trust_error = |err: ss_net::Error| ApiError::new("pin_store", err.to_string());
    if pins.policy(id).map_err(trust_error)?.is_none()
        && let Some(member) = memberships(path)?
            .into_iter()
            .find(|m| m.server_id().ok().as_ref() == Some(id))
    {
        pins.compare_exchange(id, None, member.trust)
            .map_err(trust_error)?;
    }
    Ok(())
}

fn persist(path: &Path, current: &Record) -> Result<(), ApiError> {
    let dir = path.parent().ok_or_else(|| storage_error("no directory"))?;
    let mut file = tempfile::NamedTempFile::new_in(dir).map_err(storage_error)?;
    serde_json::to_writer(file.as_file_mut(), current).map_err(storage_error)?;
    file.as_file().sync_all().map_err(storage_error)?;
    file.persist(path).map_err(storage_error)?;
    Ok(())
}

// Profile writes are serialized by AppState::operations, shared with creation,
// server removal, backup restoration, and account linking.
fn remember(path: &Path, membership: Membership) -> Result<(), ApiError> {
    let id = membership.validate()?;
    let mut current = record(path)?.ok_or_else(missing)?;
    if let Some(existing) = current
        .memberships
        .iter_mut()
        .find(|m| m.server_id().ok().as_ref() == Some(&id))
    {
        if existing.user_id != membership.user_id {
            return Err(ApiError::new(
                "membership_changed",
                "This server returned a different member for your profile. Verify the server with its host.",
            ));
        }
        if *existing == membership {
            return Ok(());
        }
        *existing = membership;
    } else {
        if current.memberships.len() >= 4096 {
            return Err(invalid());
        }
        current.memberships.push(membership);
    }
    persist(path, &current)
}

pub(crate) fn forget(path: &Path, id: &ServerId) -> Result<(), ApiError> {
    let Some(mut current) = record(path)? else {
        return Ok(());
    };
    let count = current.memberships.len();
    current
        .memberships
        .retain(|m| m.server_id().ok().as_ref() != Some(id));
    if count != current.memberships.len() {
        persist(path, &current)?;
    }
    Ok(())
}

pub(crate) async fn remember_client(state: &AppState, client: &Client) -> Result<(), ApiError> {
    let address = client.address();
    let membership = {
        let store = client.state();
        let snapshot = store.read();
        let user = snapshot.own_user.as_ref().ok_or_else(invalid)?;
        let id = ss_core::Id::from_bytes(user.id.as_ref().try_into().map_err(|_| invalid())?);
        Membership {
            host: address.host.clone(),
            port: address.port,
            https_port: address.https_port,
            name: snapshot
                .server
                .as_ref()
                .map_or_else(|| address.host.clone(), |s| s.name.clone()),
            username: user.username.clone(),
            user_id: id.to_string(),
            trust: client.verified_trust().ok_or_else(invalid)?,
        }
    };
    let path = state.profile_path.clone();
    tokio::task::spawn_blocking(move || remember(&path, membership))
        .await
        .map_err(storage_error)?
}

#[cfg(test)]
mod tests {
    use super::*;
    use ss_net::{pins::MemoryPinStore, tls::Fingerprint};

    fn member() -> Membership {
        Membership {
            host: "games.example".into(),
            name: "Friday Games".into(),
            port: 443,
            https_port: 8443,
            username: "sam".into(),
            user_id: ss_core::Id::generate().to_string(),
            trust: TrustPolicy::PinnedCertificate {
                fingerprint: Fingerprint::try_from("ab".repeat(32)).unwrap(),
            },
        }
    }

    #[test]
    fn old_profile_records_gain_only_confirmed_memberships() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profile.json");
        std::fs::write(&path, r#"{"name":"Sam","key_id":"local-item"}"#).unwrap();
        let m = member();
        assert!(snapshot(&path).unwrap().memberships.is_empty());
        remember(&path, m.clone()).unwrap();
        remember(&path, m.clone()).unwrap();
        let other = Membership {
            user_id: ss_core::Id::generate().to_string(),
            ..m.clone()
        };
        assert_eq!(
            remember(&path, other).unwrap_err().code,
            "membership_changed"
        );
        let portable = snapshot(&path).unwrap();
        assert_eq!(portable.memberships, [m.clone()]);
        let json = serde_json::to_string(&portable).unwrap();
        assert!(!json.contains("local-item"));
        let listed = server_list(Vec::new(), portable.memberships).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].1.as_deref(), Some("sam"));
        assert_eq!(listed[0].0.https_port, 8443);
        let id = m.server_id().unwrap();
        let restored = MemoryPinStore::default();
        seed_trust(&path, &restored, &id).unwrap();
        assert_eq!(restored.policy(&id).unwrap(), Some(m.trust));
        restored
            .compare_exchange(&id, Some(m.trust), TrustPolicy::PublicCertificate)
            .unwrap();
        seed_trust(&path, &restored, &id).unwrap();
        assert_eq!(
            restored.policy(&id).unwrap(),
            Some(TrustPolicy::PublicCertificate)
        );
        forget(&path, &id).unwrap();
        assert!(snapshot(&path).unwrap().memberships.is_empty());
    }

    #[test]
    fn portable_memberships_require_unique_valid_server_addresses() {
        let m = member();
        let alias = Membership {
            host: "GAMES.example.".into(),
            ..m.clone()
        };
        let mut p = PortableProfile {
            name: "Sam".into(),
            memberships: vec![m, alias],
        };
        assert_eq!(p.validate().unwrap_err().code, "invalid_profile");
        p.memberships.pop();
        p.memberships[0].https_port = 0;
        assert!(p.validate().is_err());
    }
}
