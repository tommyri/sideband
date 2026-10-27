//! A consistent database image plus the attachments tree, in one tar file.

use std::{fs::File, io::Cursor, path::Path};

use crate::db::{self, Pool};

/// Writes `socialspeak.db` and `attachments/` into `destination`.
pub async fn write(pool: &Pool, data_dir: &Path, destination: &Path) -> Result<(), Error> {
    if let Some(parent) = destination.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    let bytes = snapshot(pool).await?;
    let attachments = data_dir.join("attachments");
    let destination = destination.to_path_buf();
    tokio::task::spawn_blocking(move || pack(&destination, &bytes, &attachments))
        .await
        .map_err(|_| Error::Join)??;
    Ok(())
}

async fn snapshot(pool: &Pool) -> Result<Vec<u8>, Error> {
    let mut conn = pool.write().await;
    let buffer = conn.serialize(None).await?;
    Ok(buffer.to_vec())
}

fn pack(destination: &Path, database: &[u8], attachments: &Path) -> Result<(), Error> {
    let file = File::create(destination)?;
    let mut builder = tar::Builder::new(file);
    let mut header = tar::Header::new_gnu();
    header.set_size(u64::try_from(database.len()).unwrap_or(u64::MAX));
    header.set_mode(0o644);
    header.set_cksum();
    builder.append_data(&mut header, "socialspeak.db", Cursor::new(database))?;
    if attachments.is_dir() {
        builder.append_dir_all("attachments", attachments)?;
    }
    builder.finish()?;
    Ok(())
}

/// Backup failed.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The tar or the destination could not be written.
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// SQLite could not serialize the database.
    #[error(transparent)]
    Database(#[from] db::Error),
    /// The blocking pack task did not finish.
    #[error("backup task was cancelled")]
    Join,
}

impl From<sqlx::Error> for Error {
    fn from(error: sqlx::Error) -> Self {
        Self::Database(db::Error::from(error))
    }
}
