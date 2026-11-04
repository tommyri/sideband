//! `audit_log` rows. Rows are inserted and read. They are not updated.

use sqlx::SqliteConnection;
use ss_core::{Id, Timestamp};

use super::{Error, convert::stamp, db, model::AuditEntry};

/// Fields stored on insert. The repository sets `created_at`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewAuditEntry {
    /// Primary key.
    pub id: Id,
    /// Actor. Empty for a system action.
    pub actor_id: Option<Id>,
    /// Action name.
    pub action: String,
    /// Target kind.
    pub target_kind: String,
    /// Target id.
    pub target_id: Option<Id>,
    /// JSON text.
    pub details: String,
}

const BY_ID: &str = "SELECT id, actor_id, action, target_kind, target_id, details, created_at FROM audit_log WHERE id = ?";
const RECENT: &str = "SELECT id, actor_id, action, target_kind, target_id, details, created_at FROM audit_log ORDER BY created_at DESC, id DESC LIMIT ?";

/// Inserts an audit row.
pub async fn insert(
    conn: &mut SqliteConnection,
    entry: NewAuditEntry,
) -> Result<AuditEntry, Error> {
    let created_at = Timestamp::now().as_millis();
    db(sqlx::query(
        "INSERT INTO audit_log (id, actor_id, action, target_kind, target_id, details, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(entry.id)
    .bind(entry.actor_id)
    .bind(&entry.action)
    .bind(&entry.target_kind)
    .bind(entry.target_id)
    .bind(&entry.details)
    .bind(created_at)
    .execute(&mut *conn)
    .await)?;
    get(conn, entry.id).await?.ok_or(Error::NotFound)
}

/// Loads one audit row.
pub async fn get(conn: &mut SqliteConnection, id: Id) -> Result<Option<AuditEntry>, Error> {
    let row = db(sqlx::query_as::<_, AuditRow>(BY_ID)
        .bind(id)
        .fetch_optional(&mut *conn)
        .await)?;
    Ok(row.map(AuditRow::into_entry))
}

const PAGE: &str = "SELECT id, actor_id, action, target_kind, target_id, details, created_at FROM audit_log WHERE (?1 IS NULL OR created_at < (SELECT created_at FROM audit_log WHERE id = ?1) OR (created_at = (SELECT created_at FROM audit_log WHERE id = ?1) AND id < ?1)) AND (?3 IS NULL OR actor_id = ?3) AND (?4 IS NULL OR action = ?4) AND (?5 IS NULL OR created_at >= ?5) AND (?6 IS NULL OR created_at < ?6) ORDER BY created_at DESC, id DESC LIMIT ?2";

/// Newest `limit` audit rows. `limit = 0` is empty.
pub async fn recent(conn: &mut SqliteConnection, limit: u32) -> Result<Vec<AuditEntry>, Error> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let rows = db(sqlx::query_as::<_, AuditRow>(RECENT)
        .bind(i64::from(limit))
        .fetch_all(&mut *conn)
        .await)?;
    Ok(rows.into_iter().map(AuditRow::into_entry).collect())
}

/// Exact-match filters applied before pagination. Dates are Unix milliseconds.
#[derive(Default)]
pub struct Filter {
    /// User responsible for the action.
    pub actor: Option<Id>,
    /// Exact action name.
    pub action: Option<String>,
    /// Inclusive lower time bound.
    pub since: Option<i64>,
    /// Exclusive upper time bound.
    pub until: Option<i64>,
}

/// Newest rows strictly before `before`, when that id exists.
///
/// A missing cursor yields an empty page. `limit = 0` is empty.
pub async fn page(
    conn: &mut SqliteConnection,
    before: Option<Id>,
    limit: u32,
) -> Result<Vec<AuditEntry>, Error> {
    filtered_page(conn, before, limit, &Filter::default()).await
}

/// Page matching audit rows, ordered by timestamp and id descending.
pub async fn filtered_page(
    conn: &mut SqliteConnection,
    before: Option<Id>,
    limit: u32,
    filter: &Filter,
) -> Result<Vec<AuditEntry>, Error> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let rows = db(sqlx::query_as::<_, AuditRow>(PAGE)
        .bind(before)
        .bind(i64::from(limit))
        .bind(filter.actor)
        .bind(&filter.action)
        .bind(filter.since)
        .bind(filter.until)
        .fetch_all(&mut *conn)
        .await)?;
    Ok(rows.into_iter().map(AuditRow::into_entry).collect())
}

#[derive(Debug, sqlx::FromRow)]
struct AuditRow {
    id: Id,
    actor_id: Option<Id>,
    action: String,
    target_kind: String,
    target_id: Option<Id>,
    details: String,
    created_at: i64,
}

impl AuditRow {
    fn into_entry(self) -> AuditEntry {
        AuditEntry {
            id: self.id,
            actor_id: self.actor_id,
            action: self.action,
            target_kind: self.target_kind,
            target_id: self.target_id,
            details: self.details,
            created_at: stamp(self.created_at),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{NewAuditEntry, insert, recent};
    use crate::db::testing;

    #[tokio::test]
    async fn filters_apply_before_limit_and_cursor_with_timestamp_ties() {
        use super::{Filter, filtered_page};
        let db = testing::pool().await;
        let mut conn = db.write().await;
        let actor = testing::user(&mut conn, 1).await;
        for number in 2..=7 {
            let row = insert(
                &mut conn,
                NewAuditEntry {
                    id: testing::id(number),
                    actor_id: if number == 7 { None } else { Some(actor.id) },
                    action: if number == 6 { "other" } else { "match" }.into(),
                    target_kind: "server".into(),
                    target_id: None,
                    details: "{}".into(),
                },
            )
            .await
            .unwrap();
            sqlx::query("UPDATE audit_log SET created_at = ? WHERE id = ?")
                .bind(if number == 2 { 99_i64 } else { 100 })
                .bind(row.id)
                .execute(&mut *conn)
                .await
                .unwrap();
        }
        let filter = Filter {
            actor: Some(actor.id),
            action: Some("match".into()),
            since: Some(100),
            until: Some(101),
        };
        let rows = filtered_page(&mut conn, None, 2, &filter).await.unwrap();
        assert_eq!(
            rows.iter().map(|r| r.id).collect::<Vec<_>>(),
            [testing::id(5), testing::id(4)]
        );
        let older = filtered_page(&mut conn, Some(rows[1].id), 2, &filter)
            .await
            .unwrap();
        assert_eq!(older.len(), 1);
        assert_eq!(older[0].id, testing::id(3));
        assert_eq!(
            filtered_page(&mut conn, Some(testing::id(9)), 2, &filter)
                .await
                .unwrap()
                .len(),
            0
        );
        let system = filtered_page(
            &mut conn,
            None,
            10,
            &Filter {
                since: Some(100),
                action: Some("match".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(system.len(), 4);
        assert!(system[0].actor_id.is_none());
        let exact = Filter {
            action: Some("mat%".into()),
            ..Default::default()
        };
        assert_eq!(
            filtered_page(&mut conn, None, 10, &exact)
                .await
                .unwrap()
                .len(),
            0
        );
    }

    #[tokio::test]
    async fn insert_and_list_newest_first() {
        let db = testing::pool().await;
        let mut conn = db.write().await;
        let actor = testing::user(&mut conn, 1).await;
        let first = insert(
            &mut conn,
            NewAuditEntry {
                id: testing::id(2),
                actor_id: Some(actor.id),
                action: "user.kick".into(),
                target_kind: "user".into(),
                target_id: Some(actor.id),
                details: "{}".into(),
            },
        )
        .await
        .unwrap();
        let second = insert(
            &mut conn,
            NewAuditEntry {
                id: testing::id(3),
                actor_id: None,
                action: "server.update".into(),
                target_kind: "server".into(),
                target_id: None,
                details: "{\"name\":\"Friends\"}".into(),
            },
        )
        .await
        .unwrap();
        let rows = recent(&mut conn, 10).await.unwrap();
        assert_eq!(rows[0].id, second.id);
        assert_eq!(rows[1].id, first.id);
        assert_eq!(recent(&mut conn, 0).await.unwrap().len(), 0);
    }
}
