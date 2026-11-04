//! Shared authenticated, pinned HTTPS transport.
use crate::{Client, ConnectionState, Error};
use std::time::Duration;
/// A short-lived pinned HTTPS session. Credentials never cross the native boundary.
pub struct HttpSession {
    pub(crate) http: reqwest::Client,
    pub(crate) base: reqwest::Url,
    pub(crate) token: String,
}
impl Client {
    /// Capture the authenticated server and approved certificate for an authenticated request.
    pub(crate) async fn http_session(&self) -> Result<HttpSession, Error> {
        if self.store.read().connection != ConnectionState::Connected {
            return Err(Error::Disconnected);
        }
        let token = self.auth.read().clone().ok_or(Error::Disconnected)?;
        let pins = self.pins.clone();
        let key = self.address.id()?;
        let pin = tokio::task::spawn_blocking(move || pins.get(&key))
            .await
            .map_err(|e| Error::Storage(e.to_string()))??
            .ok_or(Error::Protocol("missing approved HTTPS pin"))?;
        let http = reqwest::Client::builder()
            .use_preconfigured_tls(ss_net::client::pinned_tls_config(pin)?)
            .https_only(true)
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(120))
            .build()?;
        let host = if self.address.host.contains(':') {
            format!("[{}]", self.address.host)
        } else {
            self.address.host.clone()
        };
        let base = reqwest::Url::parse(&format!("https://{host}:{}/", self.address.https_port))
            .map_err(|_| Error::Protocol("invalid attachment origin"))?;
        Ok(HttpSession { http, base, token })
    }
}
pub(crate) async fn read_bounded(
    response: reqwest::Response,
    limit: usize,
) -> Result<Vec<u8>, Error> {
    let mut response = response.error_for_status()?;
    if !response.status().is_success() {
        return Err(Error::Protocol("attachment redirect rejected"));
    }
    if response.content_length().is_some_and(|n| n > limit as u64) {
        return Err(Error::Protocol("attachment response is too large"));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if body.len().saturating_add(chunk.len()) > limit {
            return Err(Error::Protocol("attachment response is too large"));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}
