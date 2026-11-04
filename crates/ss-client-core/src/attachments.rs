//! Bounded, authenticated file transfers to the approved server only.
use crate::{Client, Error, http::read_bounded};
use base64::Engine;
use serde::Deserialize;
use ss_core::Id;

/// Desktop transfer ceiling; server policy may impose a lower limit.
pub const MAX_FILE_BYTES: usize = 25 * 1024 * 1024;
/// Pinned session used for attachment transfers.
pub type AttachmentSession = crate::http::HttpSession;
impl Client {
    /// Capture an authenticated session for file transfers.
    pub async fn attachment_session(&self) -> Result<AttachmentSession, Error> {
        self.http_session().await
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
