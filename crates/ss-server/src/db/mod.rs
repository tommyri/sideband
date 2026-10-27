//! SQLite schema, connection pool, and repository functions.
//!
//! [`Pool::open`] enables WAL, `synchronous=NORMAL`, foreign keys, and a 5
//! second busy timeout, then applies the embedded migrations. Writes go
//! through one connection. Reads use a separate pool.

pub mod attachments;
pub mod audit_log;
pub mod bans;
pub mod channels;
mod convert;
pub mod invites;
pub mod link_previews;
pub mod messages;
pub mod model;
pub mod read_states;
pub mod roles;
pub mod sessions;
pub mod settings;
pub mod users;
pub mod webhooks;

#[cfg(test)]
pub(crate) mod testing;

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use sqlx::{
    Connection, SqlitePool,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous},
};
use tokio::sync::Mutex;

pub use attachments::NewAttachment;
pub use audit_log::NewAuditEntry;
pub use bans::NewBan;
pub use channels::{ChannelUpdate, NewChannel};
pub use invites::NewInvite;
pub use link_previews::NewLinkPreview;
pub use messages::NewMessage;
pub use model::{
    Attachment, AuditEntry, Ban, Channel, ChannelOverride, Invite, LinkPreview, Message,
    MessageLink, PreviewStatus, ReadState, Role, Session, Setting, User, Webhook,
};
pub use roles::{ADMIN_ROLE_ID, EVERYONE_ROLE_ID, NewRole, RoleUpdate};
pub use sessions::NewSession;
pub use settings::{DESCRIPTION, ICON, NAME, REGISTRATION_INVITE, REGISTRATION_MODE};
pub use users::{NewUser, UserProfile};
pub use webhooks::NewWebhook;

/// Failures from opening the database or running a repository function.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// SQLite reported a unique or primary-key collision.
    #[error("unique value already exists")]
    Conflict,
    /// A foreign key did not match a parent row.
    #[error("referenced row does not exist")]
    ForeignKey,
    /// The statement changed no rows.
    #[error("row not found")]
    NotFound,
    /// A stored value is outside the domain encoded by the schema.
    #[error("stored value is not valid: {0}")]
    Corrupt(&'static str),
    /// The database file is missing a migration from this binary.
    #[error("database has migrations that have not been applied")]
    PendingMigrations,
    /// Applied migrations do not match the SQL embedded in this binary.
    #[error("database migrations do not match the embedded set")]
    MigrationMismatch,
    /// A domain value could not be decoded.
    #[error(transparent)]
    Data(#[from] ss_core::CoreError),
    /// The path could not be created.
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// SQLite or the migration runner failed.
    #[error(transparent)]
    Sqlx(sqlx::Error),
    /// `sqlx::migrate!` failed while applying SQL.
    #[error(transparent)]
    Migrate(#[from] sqlx::migrate::MigrateError),
}

/// One write connection plus a read pool for `path`.
pub struct Pool {
    path: PathBuf,
    write: Mutex<sqlx::SqliteConnection>,
    read: SqlitePool,
}

impl Pool {
    /// Creates the parent directory, opens `path`, and applies migrations.
    ///
    /// A second open of the same file applies nothing. The write guard is the
    /// only connection repository writes should use. Do not call [`Self::write`]
    /// again while that guard is held on the same task.
    pub async fn open(path: impl Into<PathBuf>) -> Result<Self, Error> {
        let path = path.into();
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }
        let mut write = sqlx::SqliteConnection::connect_with(&options(&path, true)).await?;
        migrator().run(&mut write).await?;
        let read = SqlitePoolOptions::new()
            .max_connections(4)
            .min_connections(0)
            .connect_with(options(&path, true))
            .await?;
        Ok(Self {
            path,
            write: Mutex::new(write),
            read,
        })
    }

    /// The database file.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The single writer. Hold it only around the repository calls it serves.
    pub async fn write(&self) -> tokio::sync::MutexGuard<'_, sqlx::SqliteConnection> {
        self.write.lock().await
    }

    /// A connection from the read pool.
    pub async fn read(&self) -> Result<sqlx::pool::PoolConnection<sqlx::Sqlite>, Error> {
        self.read.acquire().await.map_err(Error::from)
    }
}

/// Succeeds when every embedded migration is already applied with the same checksum.
///
/// Does not create the file and does not apply SQL.
pub async fn check(path: &Path) -> Result<(), Error> {
    if !path.is_file() {
        return Err(Error::PendingMigrations);
    }
    let mut conn = sqlx::SqliteConnection::connect_with(&options(path, false)).await?;
    let present: i64 = db(sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = '_sqlx_migrations'",
    )
    .fetch_one(&mut conn)
    .await)?;
    if present == 0 {
        return Err(Error::PendingMigrations);
    }
    let applied: Vec<AppliedMigration> = db(sqlx::query_as(
        "SELECT version, checksum, success FROM _sqlx_migrations ORDER BY version",
    )
    .fetch_all(&mut conn)
    .await)?;
    let migrations = migrator();
    let embedded: Vec<_> = migrations.iter().collect();
    if applied.len() != embedded.len() {
        let embedded_versions: Vec<i64> =
            embedded.iter().map(|migration| migration.version).collect();
        let applied_versions: Vec<i64> = applied.iter().map(|row| row.version).collect();
        if embedded_versions
            .iter()
            .any(|version| !applied_versions.contains(version))
        {
            return Err(Error::PendingMigrations);
        }
        return Err(Error::MigrationMismatch);
    }
    for migration in embedded {
        let Some(row) = applied.iter().find(|row| row.version == migration.version) else {
            return Err(Error::PendingMigrations);
        };
        if row.success == 0 || row.checksum.as_slice() != migration.checksum.as_ref() {
            return Err(Error::MigrationMismatch);
        }
    }
    Ok(())
}

/// Temporary migrated database. The directory is removed when this value drops.
#[cfg(test)]
pub struct TestPool {
    pool: Pool,
    _dir: tempfile::TempDir,
}

#[cfg(test)]
impl std::ops::Deref for TestPool {
    type Target = Pool;

    fn deref(&self) -> &Self::Target {
        &self.pool
    }
}

/// Opens a migrated database in a temporary directory.
#[cfg(test)]
pub async fn test_pool() -> Result<TestPool, Error> {
    let dir = tempfile::tempdir()?;
    let pool = Pool::open(dir.path().join("socialspeak.db")).await?;
    Ok(TestPool { pool, _dir: dir })
}

fn migrator() -> sqlx::migrate::Migrator {
    sqlx::migrate!("./migrations")
}

fn options(path: &Path, create: bool) -> SqliteConnectOptions {
    SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(create)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .foreign_keys(true)
        .busy_timeout(Duration::from_millis(5_000))
}

pub(crate) fn db<T>(result: Result<T, sqlx::Error>) -> Result<T, Error> {
    result.map_err(map_sql)
}

fn map_sql(error: sqlx::Error) -> Error {
    if let Some(database) = error.as_database_error() {
        if database.is_unique_violation() {
            return Error::Conflict;
        }
        if database.is_foreign_key_violation() {
            return Error::ForeignKey;
        }
    }
    Error::Sqlx(error)
}

impl From<sqlx::Error> for Error {
    fn from(error: sqlx::Error) -> Self {
        map_sql(error)
    }
}

#[derive(Debug, sqlx::FromRow)]
struct AppliedMigration {
    version: i64,
    checksum: Vec<u8>,
    success: i64,
}

#[cfg(test)]
mod tests {
    use super::{Error, Pool, check, test_pool};

    async fn pragma_i64(conn: &mut sqlx::SqliteConnection, sql: &'static str) -> i64 {
        sqlx::query_scalar(sql).fetch_one(conn).await.unwrap()
    }

    #[tokio::test]
    async fn open_sets_pragmas_migrates_seeds_and_is_idempotent() {
        let db = test_pool().await.unwrap();
        {
            let mut write = db.write().await;
            let mode: String = sqlx::query_scalar("PRAGMA journal_mode")
                .fetch_one(&mut *write)
                .await
                .unwrap();
            assert_eq!(mode.to_ascii_lowercase(), "wal");
            assert_eq!(pragma_i64(&mut write, "PRAGMA synchronous").await, 1);
            assert_eq!(pragma_i64(&mut write, "PRAGMA foreign_keys").await, 1);
            assert_eq!(pragma_i64(&mut write, "PRAGMA busy_timeout").await, 5_000);
            let roles: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM roles")
                .fetch_one(&mut *write)
                .await
                .unwrap();
            assert_eq!(roles, 2);
        }
        let mut read = db.read().await.unwrap();
        assert_eq!(pragma_i64(&mut read, "PRAGMA foreign_keys").await, 1);
        assert_eq!(pragma_i64(&mut read, "PRAGMA busy_timeout").await, 5_000);
        drop(read);

        let again = Pool::open(db.path()).await.unwrap();
        let mut write = again.write().await;
        let roles: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM roles")
            .fetch_one(&mut *write)
            .await
            .unwrap();
        assert_eq!(roles, 2);
        check(db.path()).await.unwrap();
    }

    #[tokio::test]
    async fn check_rejects_a_missing_file_and_a_changed_checksum() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing.db");
        assert!(matches!(
            check(&missing).await,
            Err(Error::PendingMigrations)
        ));

        let db = test_pool().await.unwrap();
        {
            let mut write = db.write().await;
            sqlx::query("UPDATE _sqlx_migrations SET checksum = X'00'")
                .execute(&mut *write)
                .await
                .unwrap();
        }
        assert!(matches!(
            check(db.path()).await,
            Err(Error::MigrationMismatch)
        ));
    }
}
