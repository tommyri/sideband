//! Authenticated member and ban metadata over pinned HTTPS.
use crate::{Client, Error, http::read_bounded};
use serde::{Deserialize, Serialize};
/// A member's public administrative metadata, excluding session credentials.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
#[allow(clippy::struct_excessive_bools)] // Independent account state and action capabilities in the wire projection.
pub struct ModerationMember {
    /// User ULID.
    pub id: String,
    /// Login name.
    pub username: String,
    /// Display name.
    pub name: String,
    /// Roles, highest first, including everyone.
    pub roles: Vec<String>,
    /// Account creation timestamp, decimal Unix milliseconds.
    pub joined_at: String,
    /// Most recent session activity, not an exact offline timestamp.
    pub last_session_at: Option<String>,
    /// Disabled account.
    pub disabled: bool,
    /// An active ban exists.
    pub banned: bool,
    /// Permission and hierarchy allow kicking.
    pub can_kick: bool,
    /// Permission and hierarchy allow banning.
    pub can_ban: bool,
}
/// An active ban; expired and lifted entries are omitted.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModerationBan {
    /// Ban ULID and cursor.
    pub id: String,
    /// Target user ULID.
    pub user_id: String,
    /// Target login name.
    pub username: String,
    /// Target display name.
    pub name: String,
    /// Target roles, highest first.
    pub roles: Vec<String>,
    /// Moderator-provided reason.
    pub reason: String,
    /// Decimal Unix milliseconds.
    pub created_at: String,
    /// Expiration; none is permanent.
    pub expires_at: Option<String>,
    /// Moderator login name.
    pub banned_by: String,
    /// Permission and hierarchy allow lifting the ban.
    pub can_unban: bool,
}
/// A bounded page; exactly one collection is populated.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModerationPage {
    /// Member results.
    pub members: Vec<ModerationMember>,
    /// Ban results.
    pub bans: Vec<ModerationBan>,
    /// Pass as `before` for the next page.
    pub next_cursor: Option<String>,
    /// Whether the caller may inspect bans.
    pub can_ban: bool,
}
impl Client {
    /// Read a page of members or active bans using current server authority.
    pub async fn moderation_page(
        &self,
        bans: bool,
        before: Option<ss_core::Id>,
        query: &str,
    ) -> Result<ModerationPage, Error> {
        self.http_session()
            .await?
            .moderation_page(bans, before, query)
            .await
    }
}
impl crate::http::HttpSession {
    /// Read at most 50 entries. Search text is literal, not SQL wildcard syntax.
    pub async fn moderation_page(
        &self,
        bans: bool,
        before: Option<ss_core::Id>,
        query: &str,
    ) -> Result<ModerationPage, Error> {
        if query.chars().count() > 100 {
            return Err(Error::Protocol("Search must be at most 100 characters"));
        }
        let path = if bans {
            "api/v1/moderation/bans"
        } else {
            "api/v1/moderation/members"
        };
        let mut request = self
            .http
            .get(self.base.join(path).expect("static URL"))
            .bearer_auth(&self.token)
            .timeout(std::time::Duration::from_secs(10))
            .query(&[("q", query)]);
        if let Some(before) = before {
            request = request.query(&[("before", before.to_string())]);
        }
        let body = read_bounded(request.send().await?, 1024 * 1024).await?;
        let page: ModerationPage = serde_json::from_slice(&body)?;
        if page.members.len() > 50 || page.bans.len() > 50 {
            return Err(Error::Protocol("Oversized moderation page"));
        }
        for value in page
            .members
            .iter()
            .map(|m| m.id.as_str())
            .chain(
                page.bans
                    .iter()
                    .flat_map(|b| [b.id.as_str(), b.user_id.as_str()]),
            )
            .chain(page.next_cursor.as_deref())
        {
            let _ = value
                .parse::<ss_core::Id>()
                .map_err(|_| Error::Protocol("Invalid moderation identifier"))?;
        }
        Ok(page)
    }
}
