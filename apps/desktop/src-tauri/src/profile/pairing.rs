//! Pairing state is native, short-lived and scoped to the currently open dialog.
mod crypto;
#[cfg(test)]
mod tests;
use super::{backup, catalog, membership_root, record, relay, storage_error, vault};
use crate::{
    commands::{api_error, parse_server_id},
    onboarding,
    state::AppState,
    types::{ApiError, ProbeInput},
};
use crypto::{Channel, Exchange, Invitation};
use ss_client_core::{Client, ServerAddr, pairing::PairingRelay, profile_sync::RelayKey};
use ss_core::pairing::{Claim, Guest, Offer, Owner, Reply, Request, TTL_SECONDS};
use ss_net::{pins::TrustPolicy, tls::Fingerprint};
use std::{
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};
use tauri::State;
use tokio::sync::Mutex;
use ts_rs::TS;

fn invalid() -> ApiError {
    ApiError::new(
        "pair_invalid",
        "The linking details could not be verified. Deny this request and create a new code.",
    )
}
fn ended() -> ApiError {
    ApiError::new(
        "pair_ended",
        "This request ended or expired. Create a new code on the trusted computer.",
    )
}
fn network(e: ss_client_core::Error) -> ApiError {
    match e {
        ss_client_core::Error::Http(_) | ss_client_core::Error::Timeout => ApiError::new(
            "pair_network",
            "Couldn’t reach the sync server. Check your connection and try again.",
        ),
        ss_client_core::Error::Protocol(message) => ApiError::new("pair_ended", message),
        other => api_error(other),
    }
}
fn origin(addr: &ServerAddr, policy: TrustPolicy) -> Result<String, ApiError> {
    let trust = match policy {
        TrustPolicy::PublicCertificate => "public".into(),
        TrustPolicy::PinnedCertificate { fingerprint } => fingerprint.to_string(),
    };
    Ok(format!(
        "{}|{}|{}",
        addr.id().map_err(api_error)?.as_str(),
        addr.https_port,
        trust
    ))
}
#[derive(serde::Serialize, TS)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[ts(export, export_to = "../../src/lib/generated/")]
pub enum PairView {
    Invite {
        code: String,
        target: ProbeInput,
    },
    Waiting,
    Review {
        device: String,
        phrase: String,
        memberships: usize,
    },
    Compare {
        device: String,
        phrase: String,
    },
    Ready,
    Allowed,
    Linked {
        name: String,
    },
    Closed,
}
#[derive(serde::Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/lib/generated/")]
pub enum PairAction {
    Check,
    Approve,
    Install,
    Cancel,
}

struct Source {
    client: Arc<Client>,
    key: RelayKey,
    invitation: Invitation,
    offer: Offer,
    exchange: Option<Exchange>,
    reviewed: Option<(Claim, Channel, usize)>,
    delivery: Option<String>,
    key_id: String,
}
struct Destination {
    relay: PairingRelay,
    token: [u8; 32],
    channel: Channel,
    delivery: Option<String>,
}
enum Role {
    Source(Box<Source>),
    Destination(Box<Destination>),
}
struct Session {
    id: String,
    slot: [u8; 32],
    until: Instant,
    role: Role,
}

#[derive(Default)]
pub struct Pairings(Mutex<Option<Session>>);

impl Session {
    async fn cancel(&self) -> Result<(), ApiError> {
        match &self.role {
            Role::Source(s) => {
                s.client
                    .pairing_owner(
                        &s.key,
                        &Request {
                            slot: self.slot,
                            action: Owner::Cancel,
                        },
                    )
                    .await
            }
            Role::Destination(d) => {
                d.relay
                    .device(&Request {
                        slot: self.slot,
                        action: Guest::Finish(d.token),
                    })
                    .await
            }
        }
        .map_err(network)?;
        Ok(())
    }
}
#[tauri::command]
#[allow(
    clippy::needless_pass_by_value,
    reason = "Tauri injects an owned app handle"
)]
pub fn current_pair_link(app: tauri::AppHandle) -> Option<String> {
    use tauri_plugin_deep_link::DeepLinkExt;
    app.deep_link()
        .get_current()
        .ok()
        .flatten()?
        .into_iter()
        .find(|url| url.scheme() == "sideband" && url.host_str() == Some("pair"))
        .map(|url| url.to_string())
}

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(TTL_SECONDS)
}

#[tauri::command]
pub async fn pair_start(
    state: State<'_, AppState>,
    active: State<'_, Pairings>,
    id: String,
) -> Result<PairView, ApiError> {
    let mut current = active.0.lock().await;
    let path = state.profile_path.clone();
    let (selection, key, key_id) = tokio::task::spawn_blocking(move || {
        let rec = record(&path)?.ok_or_else(catalog::missing)?;
        let choice = relay::chosen(&path, &rec.key_id)?.ok_or_else(|| {
            ApiError::new(
                "pair_relay",
                "Choose a sync server in Profile and devices first.",
            )
        })?;
        let key = RelayKey::new(&*membership_root(&path)?, parse_server_id(&choice.id)?)
            .map_err(api_error)?;
        Ok::<_, ApiError>((choice, key, rec.key_id))
    })
    .await
    .map_err(storage_error)??;
    let client = state
        .client(&parse_server_id(&selection.id)?)
        .ok_or_else(|| {
            ApiError::new(
                "pair_relay",
                "Reconnect to your chosen sync server, then create a code.",
            )
        })?;
    let trust = client.verified_trust().ok_or_else(ended)?;
    let invitation = Invitation::new(matches!(trust, TrustPolicy::PinnedCertificate { .. }))?;
    let slot = invitation.slot(&origin(client.address(), trust)?);
    let exchange = Exchange::new()?;
    let offer = invitation.offer(&slot, exchange.public);
    let until = deadline();
    client
        .pairing_owner(
            &key,
            &Request {
                slot,
                action: Owner::Start(offer.clone()),
            },
        )
        .await
        .map_err(network)?;
    let view = PairView::Invite {
        code: invitation.code(),
        target: ProbeInput {
            host: client.address().host.clone(),
            port: client.address().port,
            https_port: client.address().https_port,
        },
    };
    *current = Some(Session {
        id,
        slot,
        until,
        role: Role::Source(Box::new(Source {
            client,
            key,
            invitation,
            offer,
            exchange: Some(exchange),
            reviewed: None,
            delivery: None,
            key_id,
        })),
    });
    Ok(view)
}

#[tauri::command]
pub async fn pair_join(
    state: State<'_, AppState>,
    active: State<'_, Pairings>,
    id: String,
    target: ProbeInput,
    code: String,
    name: String,
) -> Result<PairView, ApiError> {
    let mut current = active.0.lock().await;
    if record(&state.profile_path)?.is_some() {
        return Err(backup::already_exists());
    }
    let name = name.trim();
    if !ss_core::pairing::valid_device_name(name) {
        return Err(ApiError::new(
            "pair_device",
            "Enter a name of up to 64 characters for this computer.",
        ));
    }
    let invite = Invitation::parse(&code)?;
    let mut addr = ServerAddr::new(&target.host, target.port).map_err(api_error)?;
    addr.https_port = target.https_port;
    let probe = tokio::time::timeout(
        Duration::from_secs(12),
        onboarding::probe(target, state.pins.clone()),
    )
    .await
    .map_err(|_| network(ss_client_core::Error::Timeout))??;
    if probe.previous_fingerprint.is_some() && !probe.trusted {
        return Err(ApiError::new(
            "pair_trust",
            "The sync server’s saved certificate changed. Verify it with its operator before linking.",
        ));
    }
    if !["profileSync", "linkedDeviceSignIn"]
        .iter()
        .all(|needed| probe.capabilities.iter().any(|s| s == needed))
    {
        return Err(ApiError::new(
            "pair_relay",
            "This server does not offer device linking. Check its version and sync settings.",
        ));
    }
    let pin = TrustPolicy::PinnedCertificate {
        fingerprint: Fingerprint::try_from(probe.fingerprint).map_err(|_| invalid())?,
    };
    let source_policy = if invite.pinned {
        pin
    } else {
        TrustPolicy::PublicCertificate
    };
    if !invite.pinned && !probe.trusted {
        return Err(ApiError::new(
            "pair_trust",
            "This invitation uses a verified domain. Check the relay address and its certificate.",
        ));
    }
    // Each computer keeps its own TLS policy. For an unfamiliar private
    // certificate, the lookup and offer MAC bind the exact observed fingerprint.
    let guest_policy = if probe.public_certificate {
        TrustPolicy::PublicCertificate
    } else {
        pin
    };
    let relay = PairingRelay::new(&addr, guest_policy).map_err(network)?;
    let slot = invite.slot(&origin(&addr, source_policy)?);
    let exchange = Exchange::new()?;
    let token = crypto::random()?;
    let claim = invite.claim(&slot, exchange.public, &token, name.into());
    let until = deadline();
    let Reply::Offered(offer) = relay
        .device(&Request {
            slot,
            action: Guest::Join(claim.clone()),
        })
        .await
        .map_err(network)?
    else {
        return Err(invalid());
    };
    let channel = exchange.finish(&invite, &slot, &offer, &claim, offer.key)?;
    let view = PairView::Compare {
        device: claim.name,
        phrase: channel.phrase.clone(),
    };
    *current = Some(Session {
        id,
        slot,
        until,
        role: Role::Destination(Box::new(Destination {
            relay,
            token,
            channel,
            delivery: None,
        })),
    });
    Ok(view)
}

impl Source {
    async fn send(&self, slot: [u8; 32], action: Owner) -> Result<Reply, ApiError> {
        self.client
            .pairing_owner(&self.key, &Request { slot, action })
            .await
            .map_err(network)
    }
    fn count(&self, path: &Path) -> Result<usize, ApiError> {
        let rec = record(path)?
            .filter(|r| r.key_id == self.key_id)
            .ok_or_else(ended)?;
        Ok(rec.memberships.len())
    }
    async fn check(&mut self, slot: [u8; 32], path: &Path) -> Result<PairView, ApiError> {
        match self.send(slot, Owner::Inspect).await? {
            Reply::Waiting if self.reviewed.is_none() => Ok(PairView::Waiting),
            Reply::Claimed(claim) => {
                if let Some((prior, _, _)) = &self.reviewed {
                    if *prior != claim {
                        return Err(invalid());
                    }
                } else {
                    self.invitation.verify(&slot, &self.offer, &claim)?;
                    let exchange = self.exchange.take().ok_or_else(invalid)?;
                    let channel =
                        exchange.finish(&self.invitation, &slot, &self.offer, &claim, claim.key)?;
                    self.reviewed = Some((claim, channel, self.count(path)?));
                }
                let (claim, channel, memberships) = self.reviewed.as_ref().ok_or_else(invalid)?;
                Ok(PairView::Review {
                    device: claim.name.clone(),
                    phrase: channel.phrase.clone(),
                    memberships: *memberships,
                })
            }
            _ => Err(invalid()),
        }
    }
    async fn approve(
        &mut self,
        slot: [u8; 32],
        path: &Path,
        until: Instant,
    ) -> Result<PairView, ApiError> {
        let (claim, channel, count) = self.reviewed.as_ref().ok_or_else(invalid)?;
        if self.count(path)? != *count {
            return Err(ApiError::new(
                "pair_changed",
                "Your memberships changed. Start again to review the new request.",
            ));
        }
        if self.delivery.is_none() {
            let path = path.to_owned();
            let data = tokio::task::spawn_blocking(move || backup::transfer(&path, &vault::System))
                .await
                .map_err(storage_error)??;
            self.delivery = Some(channel.seal(data)?);
        }
        if Instant::now() >= until {
            return Err(ended());
        }
        let delivery = self.delivery.as_ref().ok_or_else(invalid)?.clone();
        if !matches!(
            self.send(
                slot,
                Owner::Approve {
                    proof: claim.proof,
                    delivery
                }
            )
            .await?,
            Reply::Done
        ) {
            return Err(invalid());
        }
        Ok(PairView::Allowed)
    }
}
impl Destination {
    async fn check(&mut self, slot: [u8; 32]) -> Result<PairView, ApiError> {
        match self
            .relay
            .device(&Request {
                slot,
                action: Guest::Poll(self.token),
            })
            .await
            .map_err(network)?
        {
            Reply::Waiting => Ok(PairView::Waiting),
            Reply::Delivery(data) => {
                self.delivery = Some(data);
                Ok(PairView::Ready)
            }
            _ => Err(invalid()),
        }
    }
    async fn install(
        &self,
        slot: [u8; 32],
        path: &Path,
        until: Instant,
    ) -> Result<PairView, ApiError> {
        // Recheck approval, expiry and source membership before touching the vault.
        let Reply::Delivery(data) = self
            .relay
            .device(&Request {
                slot,
                action: Guest::Poll(self.token),
            })
            .await
            .map_err(network)?
        else {
            return Err(ended());
        };
        if Instant::now() >= until {
            return Err(ended());
        }
        if self.delivery.as_ref() != Some(&data) {
            return Err(invalid());
        }
        let bytes = self.channel.open(&data)?;
        let path = path.to_owned();
        let name = tokio::task::spawn_blocking(move || {
            backup::install_transfer(&path, &bytes, &vault::System)
        })
        .await
        .map_err(storage_error)??;
        // Installation is atomic; a lost acknowledgement leaves only this device's
        // ciphertext until expiry and must not turn a successful install into failure.
        let _ = self
            .relay
            .device(&Request {
                slot,
                action: Guest::Finish(self.token),
            })
            .await;
        Ok(PairView::Linked { name })
    }
}

#[tauri::command]
pub async fn pair_update(
    state: State<'_, AppState>,
    active: State<'_, Pairings>,
    id: String,
    action: PairAction,
) -> Result<PairView, ApiError> {
    let mut current = active.0.lock().await;
    let session = current.as_mut().filter(|s| s.id == id).ok_or_else(ended)?;
    if session.until <= Instant::now() {
        *current = None;
        return Err(ended());
    }
    if matches!(action, PairAction::Cancel) {
        let session = current.take().ok_or_else(ended)?;
        session.cancel().await?;
        return Ok(PairView::Closed);
    }
    let path = &state.profile_path;
    let slot = session.slot;
    let view = match (&mut session.role, action) {
        (Role::Source(s), PairAction::Check) => s.check(slot, path).await?,
        (Role::Source(s), PairAction::Approve) => {
            let _op = state.operations.lock().await;
            s.approve(slot, path, session.until).await?
        }
        (Role::Destination(d), PairAction::Check) => d.check(slot).await?,
        (Role::Destination(d), PairAction::Install) => {
            let _op = state.operations.lock().await;
            d.install(slot, path, session.until).await?
        }
        _ => return Err(invalid()),
    };
    if matches!(view, PairView::Linked { .. } | PairView::Allowed) {
        *current = None;
    }
    Ok(view)
}
