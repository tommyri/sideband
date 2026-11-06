//! Authenticated server profile editing through pinned HTTPS.
use crate::{Client, Error, http::read_bounded};
use serde::{Deserialize, Serialize};
/// Editable server profile. Revision is an opaque optimistic concurrency token.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerSettings {
    /// Display name.
    pub name: String,
    /// Public description.
    pub description: String,
    /// Icon attachment ULID, or no icon.
    pub icon_id: Option<String>,
    /// `invite` or `closed`.
    pub registration_mode: String,
    /// Token returned by the last read.
    pub revision: String,
}
/// Profile and stored icon metadata.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerSettingsSnapshot {
    /// Editable values.
    #[serde(flatten)]
    pub settings: ServerSettings,
    /// Stored attachment filename.
    pub icon_filename: Option<String>,
    /// Sniffed image type.
    pub icon_mime: Option<String>,
}
async fn response(response: reqwest::Response) -> Result<Vec<u8>, Error> {
    match response.status().as_u16() {
        403 => return Err(Error::Protocol("Manage server permission is required")),
        409 => {
            return Err(Error::Protocol(
                "Settings changed on the server. Reload before saving.",
            ));
        }
        400 | 413 => {
            return Err(Error::Protocol(
                "Settings or icon are invalid. Check the name, description and image limits.",
            ));
        }
        _ => {}
    }
    read_bounded(response, 32 * 1024).await
}
impl Client {
    /// Read manager-only settings over the authenticated connection.
    pub async fn server_settings(&self) -> Result<ServerSettingsSnapshot, Error> {
        self.http_session()?.server_settings().await
    }
    /// Save a revision-checked server profile.
    pub async fn save_server_settings(
        &self,
        settings: &ServerSettings,
    ) -> Result<ServerSettingsSnapshot, Error> {
        self.http_session()?.save_server_settings(settings).await
    }
    /// Upload an icon for a later settings save.
    pub async fn upload_server_icon(&self, bytes: Vec<u8>) -> Result<String, Error> {
        self.http_session()?.upload_server_icon(bytes).await
    }
}
impl crate::http::HttpSession {
    /// Read manager-only settings.
    pub async fn server_settings(&self) -> Result<ServerSettingsSnapshot, Error> {
        let session = self;
        let reply = session
            .http
            .get(
                session
                    .base
                    .join("api/v1/server/settings")
                    .expect("static URL"),
            )
            .bearer_auth(&session.token)
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await?;
        Ok(serde_json::from_slice(&response(reply).await?)?)
    }
    /// Save all fields atomically, only if the revision still matches.
    pub async fn save_server_settings(
        &self,
        settings: &ServerSettings,
    ) -> Result<ServerSettingsSnapshot, Error> {
        let session = self;
        let reply = session
            .http
            .put(
                session
                    .base
                    .join("api/v1/server/settings")
                    .expect("static URL"),
            )
            .bearer_auth(&session.token)
            .timeout(std::time::Duration::from_secs(10))
            .header("content-type", "application/json")
            .body(serde_json::to_vec(settings)?)
            .send()
            .await?;
        Ok(serde_json::from_slice(&response(reply).await?)?)
    }
    /// Upload a raster icon; it is not applied until settings are saved.
    pub async fn upload_server_icon(&self, bytes: Vec<u8>) -> Result<String, Error> {
        if bytes.is_empty() || bytes.len() > 2 * 1024 * 1024 {
            return Err(Error::Protocol("Choose an image up to 2 MiB"));
        }
        let session = self;
        let reply = session
            .http
            .post(session.base.join("api/v1/server/icon").expect("static URL"))
            .bearer_auth(&session.token)
            .header("content-type", "application/octet-stream")
            .body(bytes)
            .timeout(std::time::Duration::from_secs(30))
            .send()
            .await?;
        let uploaded: Uploaded = serde_json::from_slice(&response(reply).await?)?;
        let _ = uploaded
            .icon_id
            .parse::<ss_core::Id>()
            .map_err(|_| Error::Protocol("Invalid icon identifier"))?;
        Ok(uploaded.icon_id)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Uploaded {
    icon_id: String,
}
