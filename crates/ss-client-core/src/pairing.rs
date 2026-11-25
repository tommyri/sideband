//! Verified HTTPS transport for both sides of desktop pairing.
use crate::{Client, Error, ServerAddr, http::read_bounded, profile_sync::RelayKey};
use reqwest::{Method, RequestBuilder, StatusCode, header};
use ss_core::pairing::{Guest, MAX_DELIVERY, Owner, Reply, Request};
use ss_net::pins::TrustPolicy;
use std::time::Duration;

async fn send<T: serde::Serialize>(
    http: RequestBuilder,
    request: &Request<T>,
) -> Result<Reply, Error> {
    let response = http
        .header(header::CONTENT_TYPE, "application/json")
        .body(serde_json::to_vec(request)?)
        .send()
        .await?;
    match response.status() {
        StatusCode::OK => Ok(serde_json::from_slice(
            &read_bounded(response, MAX_DELIVERY + 2048).await?,
        )?),
        StatusCode::CONFLICT => Err(Error::Protocol(
            "This code is already in use. Create a new code on the trusted computer.",
        )),
        StatusCode::GONE => Err(Error::Protocol(
            "This request ended or expired. Create a new code on the trusted computer.",
        )),
        StatusCode::NOT_FOUND => Err(Error::Protocol(
            "This server does not offer device linking. Check its version and sync settings.",
        )),
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Err(Error::Protocol(
            "The trusted computer must reconnect with an active profile membership.",
        )),
        StatusCode::TOO_MANY_REQUESTS => Err(Error::Protocol(
            "The sync server is busy. Try again in five minutes.",
        )),
        status if status.is_server_error() => {
            Err(response.error_for_status().expect_err("5xx status").into())
        }
        _ => Err(Error::Protocol(
            "The sync server could not complete this pairing request.",
        )),
    }
}
impl Client {
    /// Perform an owner transition using this authenticated member's relay session.
    pub async fn pairing_owner(
        &self,
        key: &RelayKey,
        req: &Request<Owner>,
    ) -> Result<Reply, Error> {
        send(
            self.relay_request(key, Method::POST, "api/v1/pairing/owner")?,
            req,
        )
        .await
    }
}
/// Guest transport pinned to one verified relay origin and fixed route.
pub struct PairingRelay {
    http: reqwest::Client,
    url: reqwest::Url,
}
impl PairingRelay {
    /// Open HTTPS using the public certificate policy or the observed certificate
    /// bound to the invitation by both endpoints.
    pub fn new(addr: &ServerAddr, trust: TrustPolicy) -> Result<Self, Error> {
        let http = reqwest::Client::builder()
            .use_preconfigured_tls(ss_net::client::tls_config(trust)?)
            .https_only(true)
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(12))
            .build()?;
        let host = if addr.host.contains(':') {
            format!("[{}]", addr.host)
        } else {
            addr.host.clone()
        };
        let url = reqwest::Url::parse(&format!(
            "https://{host}:{}/api/v1/pairing/device",
            addr.https_port
        ))
        .map_err(|_| Error::Protocol("Invalid relay address."))?;
        Ok(Self { http, url })
    }
    /// Claim, poll, or acknowledge a request without an existing session.
    pub async fn device(&self, req: &Request<Guest>) -> Result<Reply, Error> {
        send(self.http.post(self.url.clone()), req).await
    }
}
