//! Attachment bytes on disk.
//!
//! Files live at `attachments/<yy>/<mm>/<ulid>` under the data directory.
//! A repeated SHA-256 reuses that blob and still inserts a new row. Unlinked
//! uploads older than an hour are deleted, except avatars and the server icon.

use std::{
    io::ErrorKind,
    path::{Component, Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use base64::Engine;
use hmac::{Hmac, Mac};
use image::ImageDecoder;
use sha2::{Digest, Sha256};
use ss_core::{Id, Timestamp};
use tokio::sync::watch;

use crate::db::{self, NewAttachment, attachments, settings};

const DAY_MS: i64 = 24 * 60 * 60 * 1000;
const KEY_FILE: &str = "attachment.key";

const DENIED_MIME: &[&str] = &[
    "application/java",
    "application/vnd.android.dex",
    "application/vnd.android.dey",
    "application/vnd.microsoft.portable-executable",
    "application/wasm",
    "application/x-executable",
    "application/x-mach-binary",
];

const DENIED_EXT: &[&str] = &[
    "com", "dll", "dylib", "exe", "msi", "scr", "so", "sys", "wasm",
];

type HmacSha256 = Hmac<Sha256>;

/// Limits for one process. Defaults match the attachment task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadPolicy {
    /// Largest accepted file. The default is 25 MiB.
    pub max_bytes: u64,
    /// Bytes one user may store in a rolling day. The default is 500 MiB.
    pub daily_bytes: u64,
    /// How long an upload may stay unlinked before cleanup.
    pub orphan_after: Duration,
    /// Lifetime of a signed download token.
    pub token_ttl: Duration,
    /// MIME types or extensions that may be stored even when they look executable.
    pub executable_allow: Vec<String>,
}

impl Default for UploadPolicy {
    fn default() -> Self {
        Self {
            max_bytes: 25 * 1024 * 1024,
            daily_bytes: 500 * 1024 * 1024,
            orphan_after: Duration::from_hours(1),
            token_ttl: Duration::from_mins(10),
            executable_allow: Vec::new(),
        }
    }
}

/// A stored upload. The blob may be shared with an older row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Saved {
    /// New attachment id.
    pub id: Id,
    /// Normalized file name.
    pub filename: String,
    /// Sniffed media type.
    pub mime: String,
    /// Size in bytes.
    pub size: u64,
    /// Pixel width after EXIF orientation, for images.
    pub width: Option<u32>,
    /// Pixel height after EXIF orientation, for images.
    pub height: Option<u32>,
}

/// Why [`Store::save`] refused or failed.
#[derive(Debug, thiserror::Error)]
pub enum SaveError {
    /// The body was empty.
    #[error("file is empty")]
    Empty,
    /// The body is over [`UploadPolicy::max_bytes`].
    #[error("file is too large")]
    TooLarge,
    /// The rolling daily total would be exceeded.
    #[error("daily upload quota exceeded")]
    Quota,
    /// The bytes or the name are an executable that is not allowlisted.
    #[error("file type is not allowed")]
    RejectedType,
    /// The data directory could not be updated.
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// SQLite rejected the row.
    #[error(transparent)]
    Database(#[from] db::Error),
}

struct Inner {
    data_dir: PathBuf,
    policy: UploadPolicy,
    key: [u8; 32],
    pool: Arc<db::Pool>,
    stop: watch::Sender<bool>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        let _ = self.stop.send(true);
        self.key.zeroize();
    }
}

/// Data directory, signing key, and the orphan cleaner.
#[derive(Clone)]
pub struct Store {
    inner: Arc<Inner>,
}

impl Store {
    /// Opens the store next to `pool`'s database file and starts orphan cleanup.
    pub async fn open(pool: Arc<db::Pool>) -> Result<Self, db::Error> {
        Self::open_with_policy(pool, UploadPolicy::default()).await
    }

    /// Like [`Self::open`], using `policy` for the upload cap.
    pub async fn open_with_policy(
        pool: Arc<db::Pool>,
        policy: UploadPolicy,
    ) -> Result<Self, db::Error> {
        let data_dir = match pool.path().parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
            _ => PathBuf::from("."),
        };
        Self::open_in(pool, data_dir, policy).await
    }

    /// Like [`Self::open`], with an explicit directory and policy.
    pub async fn open_in(
        pool: Arc<db::Pool>,
        data_dir: PathBuf,
        policy: UploadPolicy,
    ) -> Result<Self, db::Error> {
        let key = load_key(&data_dir).await?;
        let (stop, stop_rx) = watch::channel(false);
        let gc_pool = Arc::clone(&pool);
        let gc_dir = data_dir.clone();
        let orphan_after = policy.orphan_after;
        tokio::spawn(async move {
            gc_loop(gc_pool, gc_dir, orphan_after, stop_rx).await;
        });
        Ok(Self {
            inner: Arc::new(Inner {
                data_dir,
                policy,
                key,
                pool,
                stop,
            }),
        })
    }

    /// Directory that contains `attachments/` and the signing key.
    #[must_use]
    pub fn data_dir(&self) -> &Path {
        &self.inner.data_dir
    }

    /// Largest accepted upload.
    #[must_use]
    pub fn max_bytes(&self) -> u64 {
        self.inner.policy.max_bytes
    }

    /// Body cap for the HTTP extractor, including multipart framing.
    #[must_use]
    pub fn body_limit(&self) -> usize {
        let max = usize::try_from(self.inner.policy.max_bytes).unwrap_or(usize::MAX);
        max.saturating_add(256 * 1024)
    }

    /// Writes `bytes` for `user` and inserts a row.
    pub async fn save(&self, user: Id, filename: &str, bytes: &[u8]) -> Result<Saved, SaveError> {
        if bytes.is_empty() {
            return Err(SaveError::Empty);
        }
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > self.inner.policy.max_bytes {
            return Err(SaveError::TooLarge);
        }
        let filename = normalize_filename(filename);
        let mime = safe_mime(sniffed_mime(bytes));
        if is_blocked(&mime, &filename, &self.inner.policy.executable_allow) {
            return Err(SaveError::RejectedType);
        }
        let size = i64::try_from(bytes.len()).map_err(|_| SaveError::TooLarge)?;
        let hash = sha256(bytes);
        let dims = measured(bytes, &mime).await;
        let id = Id::generate();
        let temp = incoming_path(&self.inner.data_dir, id);
        write_temp(&temp, bytes).await?;
        match self
            .finish(user, id, filename, mime, size, hash, dims, &temp)
            .await
        {
            Ok(saved) => Ok(saved),
            Err(error) => {
                let _ = tokio::fs::remove_file(&temp).await;
                Err(error)
            }
        }
    }

    /// Signed path a client can put in an image tag.
    #[must_use]
    pub fn download_url(&self, id: Id, filename: &str) -> String {
        let token = self.signed_token(id);
        format!(
            "/api/v1/attachments/{id}/{}?token={token}",
            path_escape(filename)
        )
    }

    /// Whether `token` currently authorizes `id`.
    #[must_use]
    pub fn authorize(&self, id: Id, token: &str) -> bool {
        verify(&self.inner.key, id, token, Timestamp::now().as_millis())
    }

    /// Reads a blob stored at `storage_path`.
    pub async fn read(&self, storage_path: &str) -> Result<Vec<u8>, std::io::Error> {
        let path = resolve(&self.inner.data_dir, storage_path)?;
        tokio::fs::read(path).await
    }

    /// Deletes unlinked uploads older than the policy window.
    pub async fn collect_orphans(&self) -> Result<usize, db::Error> {
        let window = i64::try_from(self.inner.policy.orphan_after.as_millis()).unwrap_or(i64::MAX);
        let cutoff = Timestamp::now().as_millis().saturating_sub(window);
        remove_orphans(&self.inner.pool, &self.inner.data_dir, cutoff).await
    }

    fn signed_token(&self, id: Id) -> String {
        let ttl = i64::try_from(self.inner.policy.token_ttl.as_millis()).unwrap_or(600_000);
        let expiry = Timestamp::now().as_millis().saturating_add(ttl);
        issue_token(&self.inner.key, id, expiry)
    }

    #[allow(clippy::too_many_arguments)]
    async fn finish(
        &self,
        user: Id,
        id: Id,
        filename: String,
        mime: String,
        size: i64,
        hash: [u8; 32],
        dims: Option<(u32, u32)>,
        temp: &Path,
    ) -> Result<Saved, SaveError> {
        let mut guard = self.inner.pool.write().await;
        let since = Timestamp::now().as_millis().saturating_sub(DAY_MS);
        let stored = attachments::bytes_since(&mut guard, user, since).await?;
        let total = stored.checked_add(size).ok_or(SaveError::Quota)?;
        if u64::try_from(total).unwrap_or(u64::MAX) > self.inner.policy.daily_bytes {
            return Err(SaveError::Quota);
        }
        let (path, created) = place_blob(&mut guard, &self.inner.data_dir, id, &hash, temp).await?;
        let (width, height) = dims.unzip();
        let inserted = attachments::insert(
            &mut guard,
            NewAttachment {
                id,
                uploader_id: user,
                message_id: None,
                filename: filename.clone(),
                mime: mime.clone(),
                size,
                sha256: hash,
                width: width.map(i64::from),
                height: height.map(i64::from),
                storage_path: path.clone(),
            },
        )
        .await;
        if let Err(error) = inserted {
            if created
                && matches!(attachments::path_count(&mut guard, &path).await, Ok(0))
                && let Ok(full) = resolve(&self.inner.data_dir, &path)
            {
                let _ = tokio::fs::remove_file(full).await;
            }
            return Err(SaveError::Database(error));
        }
        Ok(Saved {
            id,
            filename,
            mime,
            size: u64::try_from(size).unwrap_or(0),
            width,
            height,
        })
    }
}

async fn place_blob(
    conn: &mut sqlx::SqliteConnection,
    data_dir: &Path,
    id: Id,
    hash: &[u8; 32],
    temp: &Path,
) -> Result<(String, bool), SaveError> {
    if let Some(path) = attachments::storage_for_hash(conn, hash).await? {
        let full = resolve(data_dir, &path)?;
        if tokio::fs::try_exists(&full).await? {
            let _ = tokio::fs::remove_file(temp).await;
        } else if let Some(parent) = full.parent() {
            tokio::fs::create_dir_all(parent).await?;
            tokio::fs::rename(temp, &full).await?;
        }
        return Ok((path, false));
    }
    let path = relative_path(id, Timestamp::now().as_millis());
    let dest = resolve(data_dir, &path)?;
    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::rename(temp, &dest).await?;
    Ok((path, true))
}

async fn write_temp(path: &Path, bytes: &[u8]) -> Result<(), std::io::Error> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(path, bytes).await
}

fn incoming_path(data_dir: &Path, id: Id) -> PathBuf {
    data_dir
        .join("attachments")
        .join(".incoming")
        .join(id.to_string())
}

async fn gc_loop(
    pool: Arc<db::Pool>,
    data_dir: PathBuf,
    orphan_after: Duration,
    mut stop: watch::Receiver<bool>,
) {
    let mut ticker = tokio::time::interval(Duration::from_secs(300));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            biased;
            result = stop.changed() => {
                if result.is_err() || *stop.borrow() {
                    break;
                }
            }
            _ = ticker.tick() => {
                let window = i64::try_from(orphan_after.as_millis()).unwrap_or(i64::MAX);
                let cutoff = Timestamp::now().as_millis().saturating_sub(window);
                if let Err(error) = remove_orphans(&pool, &data_dir, cutoff).await {
                    tracing::warn!(%error, "attachment cleanup failed");
                }
            }
        }
    }
}

async fn remove_orphans(pool: &db::Pool, data_dir: &Path, cutoff: i64) -> Result<usize, db::Error> {
    let mut guard = pool.write().await;
    let protected = icon_id(&mut guard).await?;
    let rows = attachments::unlinked_before(&mut guard, cutoff).await?;
    let mut removed = 0_usize;
    for row in rows {
        if Some(row.id) == protected {
            continue;
        }
        let path = row.storage_path.clone();
        attachments::delete(&mut guard, row.id).await?;
        if attachments::path_count(&mut guard, &path).await? == 0 {
            remove_blob(data_dir, &path).await;
        }
        removed += 1;
    }
    if removed > 0 {
        tracing::info!(removed, "deleted orphan attachments");
    }
    Ok(removed)
}

async fn remove_blob(data_dir: &Path, storage_path: &str) {
    let Ok(full) = resolve(data_dir, storage_path) else {
        return;
    };
    match tokio::fs::remove_file(&full).await {
        Ok(()) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => tracing::warn!(%error, %storage_path, "orphan file remained on disk"),
    }
}

async fn icon_id(conn: &mut sqlx::SqliteConnection) -> Result<Option<Id>, db::Error> {
    let Some(setting) = settings::get(conn, settings::ICON).await? else {
        return Ok(None);
    };
    Ok(setting.value.parse().ok())
}

async fn measured(bytes: &[u8], mime: &str) -> Option<(u32, u32)> {
    if !mime.starts_with("image/") {
        return None;
    }
    let owned = bytes.to_vec();
    tokio::task::spawn_blocking(move || decode_image_size(&owned))
        .await
        .ok()
        .flatten()
}

fn decode_image_size(bytes: &[u8]) -> Option<(u32, u32)> {
    let reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    let mut decoder = reader.into_decoder().ok()?;
    let (mut width, mut height) = decoder.dimensions();
    let orientation = decoder
        .orientation()
        .unwrap_or(image::metadata::Orientation::NoTransforms);
    if swaps_axes(orientation) {
        std::mem::swap(&mut width, &mut height);
    }
    if width == 0 || height == 0 {
        None
    } else {
        Some((width, height))
    }
}

fn swaps_axes(orientation: image::metadata::Orientation) -> bool {
    use image::metadata::Orientation::{Rotate90, Rotate90FlipH, Rotate270, Rotate270FlipH};
    matches!(
        orientation,
        Rotate90 | Rotate270 | Rotate90FlipH | Rotate270FlipH
    )
}

fn sniffed_mime(bytes: &[u8]) -> &'static str {
    infer::get(bytes).map_or("application/octet-stream", |kind| kind.mime_type())
}

fn safe_mime(mime: &str) -> String {
    let token = mime
        .bytes()
        .all(|byte| byte.is_ascii_graphic() && byte != b';' && byte != b',');
    if token && mime.contains('/') {
        mime.to_owned()
    } else {
        "application/octet-stream".to_owned()
    }
}

fn is_blocked(mime: &str, filename: &str, allow: &[String]) -> bool {
    let ext = extension(filename);
    let mime_denied = DENIED_MIME.contains(&mime);
    let ext_denied = DENIED_EXT.contains(&ext.as_str());
    if !mime_denied && !ext_denied {
        return false;
    }
    let permitted = allow.iter().any(|item| {
        let item = item.to_ascii_lowercase();
        item == mime || item == ext
    });
    !permitted
}

fn extension(filename: &str) -> String {
    match filename.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && !ext.is_empty() => ext.to_ascii_lowercase(),
        _ => String::new(),
    }
}

pub(crate) fn normalize_filename(raw: &str) -> String {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or(raw);
    let mut cleaned = String::new();
    for ch in base.chars() {
        if ch.is_control() || matches!(ch, '<' | '>' | ':' | '"' | '|' | '?' | '*') {
            cleaned.push('_');
        } else {
            cleaned.push(ch);
        }
    }
    let trimmed = cleaned.trim().trim_matches('.');
    let name: String = trimmed.chars().take(200).collect();
    if name.is_empty() {
        "file".to_owned()
    } else {
        name
    }
}

fn relative_path(id: Id, at_ms: i64) -> String {
    let (year, month) = year_month(at_ms.div_euclid(1000));
    let yy = u32::try_from(year.rem_euclid(100)).unwrap_or(0);
    format!("attachments/{yy:02}/{month:02}/{id}")
}

fn year_month(unix_secs: i64) -> (i32, u32) {
    let (year, month, _) = civil_from_days(unix_secs.div_euclid(86_400));
    (year, month)
}

/// Howard Hinnant's `civil_from_days`. `unix_secs` is whole days since 1970-01-01.
fn civil_from_days(days: i64) -> (i32, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = u64::try_from(z - era * 146_097).unwrap_or(0);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year_base = i64::try_from(yoe).unwrap_or(0) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year_base + 1 } else { year_base };
    (
        i32::try_from(year).unwrap_or(1970),
        u32::try_from(month).unwrap_or(1),
        u32::try_from(day).unwrap_or(1),
    )
}

fn resolve(data_dir: &Path, storage_path: &str) -> Result<PathBuf, std::io::Error> {
    let relative = Path::new(storage_path);
    let escapes = relative.is_absolute()
        || relative
            .components()
            .any(|component| matches!(component, Component::ParentDir));
    if escapes {
        return Err(std::io::Error::new(
            ErrorKind::InvalidInput,
            "attachment path escapes the data directory",
        ));
    }
    Ok(data_dir.join(relative))
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    let digest = Sha256::digest(bytes);
    let mut out = [0; 32];
    out.copy_from_slice(digest.as_ref());
    out
}

fn issue_token(key: &[u8], id: Id, expiry: i64) -> String {
    let mac = mac_bytes(key, id, expiry);
    let mut raw = Vec::with_capacity(40);
    raw.extend_from_slice(&expiry.to_be_bytes());
    raw.extend_from_slice(&mac);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw)
}

fn verify(key: &[u8], id: Id, token: &str, now: i64) -> bool {
    let Ok(raw) = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(token) else {
        return false;
    };
    let Some(expiry_bytes) = raw.get(..8) else {
        return false;
    };
    let Some(tag) = raw.get(8..) else {
        return false;
    };
    if tag.len() != 32 {
        return false;
    }
    let mut expiry_buf = [0; 8];
    expiry_buf.copy_from_slice(expiry_bytes);
    let expiry = i64::from_be_bytes(expiry_buf);
    let Ok(mut mac) = HmacSha256::new_from_slice(key) else {
        return false;
    };
    let id_bytes = id.as_bytes();
    mac.update(&id_bytes);
    mac.update(&expiry_buf);
    if mac.verify_slice(tag).is_err() {
        return false;
    }
    expiry > now
}

fn mac_bytes(key: &[u8], id: Id, expiry: i64) -> [u8; 32] {
    let mut mac = HmacSha256::new_from_slice(key).unwrap_or_else(|_| {
        HmacSha256::new_from_slice(&[0; 32]).expect("hmac accepts a 32-byte key")
    });
    let id_bytes = id.as_bytes();
    mac.update(&id_bytes);
    mac.update(&expiry.to_be_bytes());
    let bytes = mac.finalize().into_bytes();
    let mut out = [0; 32];
    out.copy_from_slice(bytes.as_ref());
    out
}

fn path_escape(filename: &str) -> String {
    let mut out = String::new();
    for byte in filename.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(char::from(byte));
            }
            _ => {
                use std::fmt::Write;
                let _ = write!(out, "%{byte:02X}");
            }
        }
    }
    out
}

async fn load_key(data_dir: &Path) -> Result<[u8; 32], std::io::Error> {
    tokio::fs::create_dir_all(data_dir).await?;
    let path = data_dir.join(KEY_FILE);
    if let Ok(bytes) = tokio::fs::read(&path).await {
        let bytes = zeroize::Zeroizing::new(bytes);
        if bytes.len() == 32 {
            let mut key = [0; 32];
            key.copy_from_slice(&bytes);
            return Ok(key);
        }
    }
    let mut key = [0; 32];
    getrandom::getrandom(&mut key).map_err(|_| std::io::Error::other("os random failed"))?;
    write_private(&path, &key).await?;
    restrict_key(&path).await?;
    Ok(key)
}

/// Creates or truncates `path` as owner-only before any byte is written, so
/// the key is never readable by other users, even briefly.
async fn write_private(path: &Path, bytes: &[u8]) -> Result<(), std::io::Error> {
    use tokio::io::AsyncWriteExt;
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(path).await?;
    file.write_all(bytes).await?;
    file.sync_all().await
}

async fn restrict_key(path: &Path) -> Result<(), std::io::Error> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).await?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        decode_image_size, is_blocked, issue_token, normalize_filename, sniffed_mime, verify,
        year_month,
    };

    const PNG_1X1: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90,
        0x77, 0x53, 0xDE, 0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0xF8,
        0xCF, 0xC0, 0x00, 0x00, 0x03, 0x01, 0x01, 0x00, 0xC9, 0xFE, 0x92, 0xEF, 0x00, 0x00, 0x00,
        0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];

    #[test]
    fn calendar_months_follow_unix_days() {
        assert_eq!(year_month(0), (1970, 1));
        assert_eq!(year_month(31 * 86_400), (1970, 2));
        assert_eq!(year_month(1_700_000_000), (2023, 11));
        assert_eq!(year_month(1_793_059_200), (2026, 10));
    }

    #[test]
    fn filenames_lose_directories_and_control_bytes() {
        assert_eq!(normalize_filename("../../etc/passwd"), "passwd");
        assert_eq!(normalize_filename(".."), "file");
        assert_eq!(normalize_filename("   "), "file");
        assert_eq!(normalize_filename("notes.txt"), "notes.txt");
        assert_eq!(normalize_filename("a/b\\c.txt"), "c.txt");
    }

    #[test]
    fn executables_are_rejected_until_allowlisted() {
        let bytes = b"MZ\x90\x00not a real program";
        let mime = sniffed_mime(bytes);
        assert!(
            is_blocked(mime, "photo.png", &[]),
            "sniffed {mime} was allowed"
        );
        assert!(is_blocked("text/plain", "run.exe", &[]));
        assert!(!is_blocked("text/plain", "run.exe", &["exe".to_owned()]));
        assert!(!is_blocked(mime, "photo.png", &[mime.to_owned()]));
        assert!(!is_blocked("image/png", "pixel.png", &[]));
    }

    #[test]
    fn png_dimensions_and_exif_rotation_swap_axes() {
        assert_eq!(decode_image_size(PNG_1X1), Some((1, 1)));
        let mut encoded = Vec::new();
        let mut jpeg = image::codecs::jpeg::JpegEncoder::new(&mut encoded);
        jpeg.encode(
            &[255, 0, 0, 0, 0, 255],
            2,
            1,
            image::ExtendedColorType::Rgb8,
        )
        .unwrap();
        assert_eq!(decode_image_size(&encoded), Some((2, 1)));
        let turned = with_orientation(&encoded, 6);
        assert_eq!(decode_image_size(&turned), Some((1, 2)));
    }

    #[test]
    fn expired_tokens_fail_the_mac_check() {
        let key = [9_u8; 32];
        let id = ss_core::Id::from_bytes([1; 16]);
        let token = issue_token(&key, id, 50);
        assert!(verify(&key, id, &token, 49));
        assert!(!verify(&key, id, &token, 50));
        assert!(!verify(&key, id, &token, 51));
        assert!(!verify(&[8_u8; 32], id, &token, 49));
    }

    fn with_orientation(jpeg: &[u8], orientation: u16) -> Vec<u8> {
        let mut app1 = Vec::new();
        app1.extend_from_slice(b"Exif\0\0");
        app1.extend_from_slice(b"II");
        app1.extend_from_slice(&42_u16.to_le_bytes());
        app1.extend_from_slice(&8_u32.to_le_bytes());
        app1.extend_from_slice(&1_u16.to_le_bytes());
        app1.extend_from_slice(&0x0112_u16.to_le_bytes());
        app1.extend_from_slice(&3_u16.to_le_bytes());
        app1.extend_from_slice(&1_u32.to_le_bytes());
        app1.extend_from_slice(&orientation.to_le_bytes());
        app1.extend_from_slice(&0_u16.to_le_bytes());
        app1.extend_from_slice(&0_u32.to_le_bytes());
        let mut segment = Vec::new();
        segment.extend_from_slice(&[0xFF, 0xE1]);
        let len = u16::try_from(app1.len() + 2).unwrap_or(0);
        segment.extend_from_slice(&len.to_be_bytes());
        segment.extend_from_slice(&app1);
        let mut out = Vec::with_capacity(jpeg.len() + segment.len());
        let (head, tail) = jpeg.split_at(2);
        out.extend_from_slice(head);
        out.extend_from_slice(&segment);
        out.extend_from_slice(tail);
        out
    }
}
