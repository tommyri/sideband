//! Bounded, authenticated file transfers to the approved server only.
use crate::{Client, ConnectionState, Error};
use base64::Engine;
use serde::Deserialize;
use ss_core::Id;
use std::time::Duration;

/// Desktop transfer ceiling; server policy may impose a lower limit.
pub const MAX_FILE_BYTES: usize = 25 * 1024 * 1024;
/// A short-lived transfer session. Credentials never cross the native boundary.
pub struct AttachmentSession {
    http: reqwest::Client,
    base: reqwest::Url,
    token: String,
}
impl Client {
    /// Capture the authenticated server and approved certificate for a transfer.
    pub async fn attachment_session(&self) -> Result<AttachmentSession, Error> {
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
        Ok(AttachmentSession { http, base, token })
    }
}
impl AttachmentSession {
    /// Upload exactly one file. The server validates channel access, type and quota.
    pub async fn upload(&self, channel: Id, filename: String, bytes: Vec<u8>) -> Result<Id, Error> {
        if bytes.is_empty() || bytes.len() > MAX_FILE_BYTES {
            return Err(Error::Protocol("file must be between 1 byte and 25 MiB"));
        }
        let form = reqwest::multipart::Form::new()
            .text("channel_id", channel.to_string())
            .part(
                "file",
                reqwest::multipart::Part::bytes(bytes).file_name(filename),
            );
        let response = self
            .http
            .post(self.base.join("api/v1/attachments").expect("static URL"))
            .bearer_auth(&self.token)
            .multipart(form)
            .send()
            .await?;
        let body = read_bounded(response, 64 * 1024).await?;
        let uploaded: Uploaded = serde_json::from_slice(&body)?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(uploaded.attachment_id)
            .map_err(|_| Error::Protocol("invalid attachment id"))?;
        Id::try_from(bytes.as_slice()).map_err(|_| Error::Protocol("invalid attachment id"))
    }
    /// Download a server attachment; arbitrary URLs and redirects are never followed.
    pub async fn download(&self, id: Id, filename: &str) -> Result<Vec<u8>, Error> {
        let mut url = self.base.clone();
        url.path_segments_mut()
            .map_err(|()| Error::Protocol("invalid attachment origin"))?
            .extend(["api", "v1", "attachments", &id.to_string(), filename]);
        let response = self.http.get(url).bearer_auth(&self.token).send().await?;
        read_bounded(response, MAX_FILE_BYTES).await
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Uploaded {
    attachment_id: String,
}
async fn read_bounded(response: reqwest::Response, limit: usize) -> Result<Vec<u8>, Error> {
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
