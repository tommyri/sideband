//! Pinned HTTPS search, with result visibility checked against the current client state.
use crate::{Client, Error, http::read_bounded};
use serde::{Deserialize, Serialize};
use ss_core::Id;
/// Search request. Dates are UTC milliseconds, exclusive bounds.
#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchRequest {
    /// Literal words, not an FTS expression.
    pub q: String,
    /// Conversation ULID or all visible conversations.
    pub channel: Option<String>,
    /// Author ULID.
    pub from: Option<String>,
    /// Strictly older timestamp.
    pub before: Option<i64>,
    /// Strictly newer timestamp.
    pub after: Option<i64>,
    /// `link`, `file` or `image`.
    pub has: Option<String>,
}
/// Search result text. Excerpt markers are interpreted as text, never HTML.
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchHit {
    /// Message ULID.
    pub id: String,
    /// Conversation ULID.
    pub channel_id: String,
    /// Author ULID for account posts.
    pub author_id: Option<String>,
    /// Display name.
    pub author_name: String,
    /// Milliseconds represented as decimal text.
    pub created_at: String,
    /// U+0001 and U+0002 delimit matches inside a plain-text excerpt.
    pub excerpt: String,
}
/// Detached search capability; networking never holds the desktop clients mutex.
pub struct SearchSession {
    session: crate::http::HttpSession,
    store: crate::StateStore,
    generation: u64,
}
impl Client {
    /// Capture the authenticated connection and current visibility store.
    pub async fn search_session(&self) -> Result<SearchSession, Error> {
        let generation = self.store.read().generation;
        let session = self.http_session().await?;
        Ok(SearchSession {
            generation,
            store: self.state(),
            session,
        })
    }
    /// Search current visible conversations.
    pub async fn search_messages(&self, query: &SearchRequest) -> Result<Vec<SearchHit>, Error> {
        self.search_session().await?.search_messages(query).await
    }
}
impl SearchSession {
    /// Search current visible conversations. Results are not persisted in the message cache.
    pub async fn search_messages(&self, query: &SearchRequest) -> Result<Vec<SearchHit>, Error> {
        let generation = self.generation;
        let session = &self.session;
        let mut url = session.base.join("api/v1/search").expect("static URL");
        {
            let mut args = url.query_pairs_mut();
            args.append_pair("q", &query.q).append_pair("limit", "50");
            for (key, value) in [
                ("channel", query.channel.as_ref()),
                ("from", query.from.as_ref()),
                ("has", query.has.as_ref()),
            ] {
                if let Some(value) = value {
                    args.append_pair(key, value);
                }
            }
            if let Some(value) = query.before {
                args.append_pair("before", &value.to_string());
            }
            if let Some(value) = query.after {
                args.append_pair("after", &value.to_string());
            }
        }
        let reply = session
            .http
            .get(url)
            .bearer_auth(&session.token)
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await?;
        if reply.status().as_u16() == 400 {
            return Err(Error::Protocol(
                "Use 1 to 16 search words (at most 256 characters) and valid filters",
            ));
        }
        let body = read_bounded(reply, 512 * 1024).await?;
        let rows: Vec<SearchHit> = serde_json::from_slice(&body)?;
        if rows.len() > 50 {
            return Err(Error::Protocol("oversized search page"));
        }
        let state = self.store.read();
        if state.generation != generation || state.connection != crate::ConnectionState::Connected {
            return Err(Error::Disconnected);
        }
        let mut result = Vec::new();
        for row in rows {
            let _ = row
                .id
                .parse::<Id>()
                .map_err(|_| Error::Protocol("invalid search message"))?;
            let channel = row
                .channel_id
                .parse::<Id>()
                .map_err(|_| Error::Protocol("invalid search channel"))?;
            if let Some(author) = &row.author_id {
                let _ = author
                    .parse::<Id>()
                    .map_err(|_| Error::Protocol("invalid search author"))?;
            }
            let stamp = row
                .created_at
                .parse::<i64>()
                .map_err(|_| Error::Protocol("invalid search date"))?;
            if !(-8_640_000_000_000_000..=8_640_000_000_000_000).contains(&stamp) {
                return Err(Error::Protocol("invalid search date"));
            }
            if state.channels.contains_key(&channel) || state.direct_messages.contains_key(&channel)
            {
                result.push(row);
            }
        }
        Ok(result)
    }
}
