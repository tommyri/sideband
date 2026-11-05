//! FTS5 search over live messages, restricted to a caller-supplied visibility set.
use super::{Error, db};
use sqlx::{QueryBuilder, Sqlite, SqliteConnection};
use ss_core::Id;
use std::fmt::Write;
/// Validated filters. Dates are UTC Unix milliseconds, exclusive bounds.
#[derive(Default)]
pub struct Filter {
    /// Literal search terms joined with AND.
    pub query: String,
    /// Visible channel subset.
    pub channel: Option<Id>,
    /// Author account.
    pub author: Option<Id>,
    /// Strictly older than this timestamp.
    pub before: Option<i64>,
    /// Strictly newer than this timestamp.
    pub after: Option<i64>,
    /// `link`, `file`, or `image`.
    pub has: Option<String>,
    /// Maximum results.
    pub limit: u32,
}
/// Matched message metadata. Snippet delimiters are text markers, never HTML.
#[derive(sqlx::FromRow)]
pub struct Hit {
    /// Message id.
    pub id: Id,
    /// Conversation id.
    pub channel_id: Id,
    /// Account id, absent for webhook posts.
    pub author_id: Option<Id>,
    /// Current author display name or webhook name.
    pub author_name: String,
    /// Message timestamp.
    pub created_at: i64,
    /// Highlighted excerpt; U+0001/U+0002 surround matching terms.
    pub excerpt: String,
}
/// Quote every token so syntax such as OR, quotes and asterisks cannot become FTS operators.
pub fn literal_query(input: &str) -> Option<String> {
    let tokens: Vec<_> = input
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|v| !v.is_empty())
        .collect();
    if tokens.is_empty() || tokens.len() > 16 || input.chars().count() > 256 {
        return None;
    }
    Some(
        tokens
            .into_iter()
            .map(|word| format!("\"{word}\""))
            .collect::<Vec<_>>()
            .join(" AND "),
    )
}
/// Search only visible channels before newest-first ordering and limiting. Empty visibility returns no rows.
pub async fn search(
    conn: &mut SqliteConnection,
    filter: &Filter,
    visible: &[Id],
) -> Result<Vec<Hit>, Error> {
    if visible.is_empty() {
        return Ok(Vec::new());
    }
    // JSON avoids SQLite's bind-parameter limit for large server channel trees.
    let allowed: Vec<String> = visible
        .iter()
        .map(|id| {
            let mut hex = String::with_capacity(32);
            for byte in id.as_bytes() {
                write!(hex, "{byte:02X}").expect("write to String");
            }
            hex
        })
        .collect();
    let allowed = serde_json::to_string(&allowed).map_err(|_| Error::Corrupt("search channels"))?;
    let mut query = QueryBuilder::<Sqlite>::new(
        "SELECT m.id, m.channel_id, m.author_id, COALESCE(m.webhook_name, NULLIF(u.display_name, ''), u.username, 'Unknown author') AS author_name, m.created_at, snippet(message_search, 0, char(1), char(2), '…', 48) AS excerpt FROM message_search JOIN messages m ON m.rowid=message_search.rowid LEFT JOIN users u ON u.id=m.author_id WHERE message_search MATCH ",
    );
    query
        .push_bind(&filter.query)
        .push(" AND m.deleted_at IS NULL AND hex(m.channel_id) IN (SELECT value FROM json_each(")
        .push_bind(allowed)
        .push("))");
    if let Some(channel) = filter.channel {
        query.push(" AND m.channel_id=").push_bind(channel);
    }
    if let Some(author) = filter.author {
        query.push(" AND m.author_id=").push_bind(author);
    }
    if let Some(before) = filter.before {
        query.push(" AND m.created_at<").push_bind(before);
    }
    if let Some(after) = filter.after {
        query.push(" AND m.created_at>").push_bind(after);
    }
    match filter.has.as_deref() {
        Some("link") => {
            query.push(" AND (EXISTS (SELECT 1 FROM message_links l WHERE l.message_id=m.id) OR instr(lower(m.content),'https://')>0 OR instr(lower(m.content),'http://')>0)");
        }
        Some("file") => {
            query.push(" AND EXISTS (SELECT 1 FROM attachments a WHERE a.message_id=m.id)");
        }
        Some("image") => {
            query.push(" AND EXISTS (SELECT 1 FROM attachments a WHERE a.message_id=m.id AND a.mime LIKE 'image/%')");
        }
        _ => {}
    }
    // Global BM25 statistics include hidden channels and other users' DMs.
    // Visible-row timestamps and IDs cannot reveal those corpus changes through ranking.
    query
        .push(" ORDER BY m.created_at DESC, m.id DESC LIMIT ")
        .push_bind(filter.limit.min(100));
    db(query.build_query_as::<Hit>().fetch_all(conn).await)
}
/// Rebuild the FTS index transactionally from live message content.
pub async fn reindex(conn: &mut SqliteConnection) -> Result<(), Error> {
    db(
        sqlx::query("INSERT INTO message_search(message_search) VALUES('rebuild')")
            .execute(conn)
            .await,
    )?;
    Ok(())
}
