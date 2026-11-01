//! Argon2id password hashing.
//!
//! Parameters are the OWASP recommendation: 19 MiB, two iterations, one lane.
//! Hashing and verification run on Tokio's blocking pool so a login cannot
//! stall the runtime thread that also reads QUIC.

use super::Error;
use argon2::{
    Algorithm, Argon2, Params, Version,
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
};
use zeroize::Zeroizing;

/// Memory cost in kibibytes. 19 MiB.
const MEMORY_KIB: u32 = 19_456;
const TIME_COST: u32 = 2;
const PARALLELISM: u32 = 1;

/// Hashes `password` off the runtime.
pub(crate) async fn hash(password: &str) -> Result<String, Error> {
    let password = Zeroizing::new(password.to_owned());
    let probe = take_probe();
    blocking(move || {
        probe.wait();
        hash_sync(&password)
    })
    .await
}

/// Checks `password` against a PHC string, off the runtime.
///
/// A mismatch is `Ok(false)`. A malformed hash is [`Error::Password`].
pub(crate) async fn verify(password: &str, stored: &str) -> Result<bool, Error> {
    let password = Zeroizing::new(password.to_owned());
    let stored = stored.to_owned();
    let probe = take_probe();
    blocking(move || {
        probe.wait();
        verify_sync(&password, &stored)
    })
    .await
}

fn hash_sync(password: &str) -> Result<String, Error> {
    let mut salt_bytes = [0_u8; 16];
    getrandom::getrandom(&mut salt_bytes).map_err(|_| Error::Random)?;
    let salt = SaltString::encode_b64(&salt_bytes).map_err(|_| Error::Password)?;
    let hash = hasher()?
        .hash_password(password.as_bytes(), &salt)
        .map_err(|_| Error::Password)?;
    Ok(hash.to_string())
}

fn verify_sync(password: &str, stored: &str) -> Result<bool, Error> {
    let parsed = PasswordHash::new(stored).map_err(|_| Error::Password)?;
    match hasher()?.verify_password(password.as_bytes(), &parsed) {
        Ok(()) => Ok(true),
        Err(argon2::password_hash::Error::Password) => Ok(false),
        Err(_) => Err(Error::Password),
    }
}

fn hasher() -> Result<Argon2<'static>, Error> {
    // 19 MiB, t=2, p=1 are inside the crate's documented bounds.
    let params =
        Params::new(MEMORY_KIB, TIME_COST, PARALLELISM, None).map_err(|_| Error::Password)?;
    Ok(Argon2::new(Algorithm::Argon2id, Version::V0x13, params))
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, Error> + Send + 'static,
) -> Result<T, Error> {
    match tokio::task::spawn_blocking(work).await {
        Ok(result) => result,
        Err(_join) => Err(Error::Join),
    }
}

/// Test hook carried onto the blocking thread. Production builds hold nothing.
struct Probe {
    #[cfg(test)]
    inner: Option<Armed>,
}

#[cfg(test)]
struct Armed {
    thread: tokio::sync::oneshot::Sender<std::thread::ThreadId>,
    release: std::sync::mpsc::Receiver<()>,
}

impl Probe {
    fn wait(self) {
        #[cfg(test)]
        if let Some(armed) = self.inner {
            let _ = armed.thread.send(std::thread::current().id());
            let _ = armed.release.recv();
        }
        #[cfg(not(test))]
        let _ = self;
    }
}

fn take_probe() -> Probe {
    #[cfg(test)]
    {
        Probe {
            inner: PROBE.with(|slot| slot.borrow_mut().take()),
        }
    }
    #[cfg(not(test))]
    {
        Probe {}
    }
}

#[cfg(test)]
thread_local! {
    static PROBE: std::cell::RefCell<Option<Armed>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn arm() -> (
    tokio::sync::oneshot::Receiver<std::thread::ThreadId>,
    std::sync::mpsc::Sender<()>,
) {
    let (thread_tx, thread_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    PROBE.with(|slot| {
        *slot.borrow_mut() = Some(Armed {
            thread: thread_tx,
            release: release_rx,
        });
    });
    (thread_rx, release_tx)
}

#[cfg(test)]
mod tests {
    use std::{thread::ThreadId, time::Duration};

    use super::{arm, hash, verify};

    #[tokio::test(flavor = "current_thread")]
    async fn hash_and_verify_run_off_the_runtime_worker() {
        let stored = hash_off_worker().await;
        assert!(stored.starts_with("$argon2id$"));
        assert!(stored.contains("m=19456,t=2,p=1"));
        assert!(verify_off_worker(&stored, "password-ok").await);
        assert!(!verify_off_worker(&stored, "password-no").await);
    }

    async fn hash_off_worker() -> String {
        let worker = std::thread::current().id();
        let (thread_rx, release_tx) = arm();
        let job = tokio::spawn(hash("password-ok"));
        release_after_ping(worker, thread_rx, release_tx).await;
        job.await.expect("join").expect("hash")
    }

    async fn verify_off_worker(stored: &str, password: &str) -> bool {
        let stored = stored.to_owned();
        let password = password.to_owned();
        let worker = std::thread::current().id();
        let (thread_rx, release_tx) = arm();
        let job = tokio::spawn(async move { verify(&password, &stored).await });
        release_after_ping(worker, thread_rx, release_tx).await;
        job.await.expect("join").expect("verify")
    }

    async fn release_after_ping(
        worker: ThreadId,
        thread_rx: tokio::sync::oneshot::Receiver<ThreadId>,
        release_tx: std::sync::mpsc::Sender<()>,
    ) {
        let hasher = tokio::time::timeout(Duration::from_secs(5), thread_rx)
            .await
            .expect("argon2 started")
            .expect("probe sent");
        assert_ne!(hasher, worker);
        let ping = tokio::spawn(async { std::thread::current().id() })
            .await
            .expect("ping");
        assert_eq!(ping, worker);
        release_tx.send(()).expect("release");
    }
}
