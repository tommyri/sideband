//! Volatile pairing mailboxes. A relay restart invalidates open invitations.
use super::profile_sync::{Member, Relay, authenticated, authorize};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::post,
};
use sha2::{Digest, Sha256};
use sqlx::Connection;
use ss_core::pairing::{Claim, Guest, MAX_DELIVERY, Offer, Owner, Reply, Request, TTL_SECONDS};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

#[cfg(test)]
mod tests;

const GONE: StatusCode = StatusCode::GONE;
struct Slot {
    member: Member,
    until: Instant,
    offer: Offer,
    claim: Option<Claim>,
    delivery: Option<String>,
}
#[derive(Clone)]
struct Service {
    relay: Relay,
    mailboxes: Mailboxes,
}
#[derive(Clone, Default)]
struct Mailboxes(Arc<Mutex<HashMap<[u8; 32], Slot>>>);
pub(super) fn router(relay: Relay) -> Router {
    Router::new()
        .route("/api/v1/pairing/owner", post(owner))
        .layer(DefaultBodyLimit::max(MAX_DELIVERY + 2048))
        .merge(
            Router::new()
                .route("/api/v1/pairing/device", post(device))
                .layer(DefaultBodyLimit::max(2048)),
        )
        .with_state(Service {
            relay,
            mailboxes: Mailboxes::default(),
        })
}
impl Mailboxes {
    fn slots(&self) -> Result<std::sync::MutexGuard<'_, HashMap<[u8; 32], Slot>>, StatusCode> {
        let mut slots = self
            .0
            .lock()
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        let now = Instant::now();
        slots.retain(|_, s| s.until > now);
        Ok(slots)
    }
    fn owner_action(&self, member: Member, req: Request<Owner>) -> Result<Reply, StatusCode> {
        let mut slots = self.slots()?;
        if let Owner::Start(offer) = req.action {
            if slots.contains_key(&req.slot) {
                return Err(StatusCode::CONFLICT);
            }
            slots.retain(|_, s| s.member.user != member.user);
            if slots.len() >= 64 {
                return Err(StatusCode::TOO_MANY_REQUESTS);
            }
            slots.insert(
                req.slot,
                Slot {
                    member,
                    until: Instant::now() + Duration::from_secs(TTL_SECONDS),
                    offer,
                    claim: None,
                    delivery: None,
                },
            );
            return Ok(Reply::Waiting);
        }
        let slot = slots
            .get_mut(&req.slot)
            .filter(|s| s.member == member)
            .ok_or(GONE)?;
        match req.action {
            Owner::Start(_) => unreachable!(),
            Owner::Inspect => Ok(slot.claim.clone().map_or(Reply::Waiting, Reply::Claimed)),
            Owner::Approve { proof, delivery } => {
                if slot.claim.as_ref().is_none_or(|c| c.proof != proof)
                    || delivery.is_empty()
                    || delivery.len() > MAX_DELIVERY
                {
                    return Err(StatusCode::BAD_REQUEST);
                }
                if slot
                    .delivery
                    .as_ref()
                    .is_some_and(|previous| previous != &delivery)
                {
                    return Err(StatusCode::CONFLICT);
                }
                slot.delivery = Some(delivery);
                Ok(Reply::Done)
            }
            Owner::Cancel => {
                slots.remove(&req.slot);
                Ok(Reply::Done)
            }
        }
    }
    fn guest_action(&self, member: &Member, req: Request<Guest>) -> Result<Reply, StatusCode> {
        let mut slots = self.slots()?;
        let slot = slots
            .get_mut(&req.slot)
            .filter(|s| &s.member == member)
            .ok_or(GONE)?;
        match req.action {
            Guest::Join(claim) => {
                if !ss_core::pairing::valid_device_name(&claim.name) {
                    return Err(StatusCode::BAD_REQUEST);
                }
                if slot.claim.as_ref().is_some_and(|c| c != &claim) {
                    return Err(StatusCode::CONFLICT);
                }
                slot.claim = Some(claim);
                Ok(Reply::Offered(slot.offer.clone()))
            }
            Guest::Poll(token) | Guest::Finish(token) => {
                let reader: [u8; 32] = Sha256::digest(token).into();
                if slot.claim.as_ref().is_none_or(|c| c.reader != reader) {
                    return Err(GONE);
                }
                if matches!(req.action, Guest::Finish(_)) {
                    slots.remove(&req.slot);
                    return Ok(Reply::Done);
                }
                Ok(slot
                    .delivery
                    .clone()
                    .map_or(Reply::Waiting, Reply::Delivery))
            }
        }
    }
}
fn response(reply: Reply) -> Response {
    ([(header::CACHE_CONTROL, "no-store")], Json(reply)).into_response()
}
async fn owner(
    State(s): State<Service>,
    headers: HeaderMap,
    Json(req): Json<Request<Owner>>,
) -> Result<Response, StatusCode> {
    let member = authenticated(&s.relay, &headers).await?;
    let mut guard = s
        .relay
        .pool
        .read()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut tx = guard
        .begin()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    authorize(&mut tx, &member).await?;
    s.mailboxes.owner_action(member, req).map(response)
}
async fn device(
    State(s): State<Service>,
    Json(req): Json<Request<Guest>>,
) -> Result<Response, StatusCode> {
    let member = s
        .mailboxes
        .slots()?
        .get(&req.slot)
        .ok_or(GONE)?
        .member
        .clone();
    let mut guard = s
        .relay
        .pool
        .read()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut tx = guard
        .begin()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    authorize(&mut tx, &member).await?;
    s.mailboxes.guest_action(&member, req).map(response)
}
