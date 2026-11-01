//! Opaque streaming fan-out: bounded chunks per downstream group, never wait
//! for a viewer on the sender's read path. FIN remains cancellable until ACK.
use super::{Cancel, Counters, GROUP_BUDGET, Id, Ordering, Streams};
use bytes::{Bytes, BytesMut};
use ss_net::media::stream::{RESET_GROUP, read_header};
use std::{sync::Arc, time::Duration};
use tokio::{sync::mpsc, task::JoinSet, time::timeout};

const CHUNK: usize = 16 * 1024;
const QUEUE_CHUNKS: usize = 8;
const MAX_GROUP_BYTES: usize = 32 * 1024 * 1024;
const IDLE: Duration = Duration::from_secs(5);
const LIFETIME: Duration = Duration::from_secs(30);
struct Sink {
    tx: mpsc::Sender<Bytes>,
    cancel: Cancel,
}
struct SourceGuard {
    cancel: Cancel,
    complete: bool,
}
impl Drop for SourceGuard {
    fn drop(&mut self) {
        if !self.complete {
            self.cancel.stop();
        }
    }
}
struct Output(quinn::SendStream);
impl Drop for Output {
    fn drop(&mut self) {
        let _ = self.0.reset(RESET_GROUP.into());
    }
}

impl Streams {
    pub(crate) async fn run(
        self: Arc<Self>,
        conn: Id,
        connection: quinn::Connection,
    ) -> quinn::ConnectionError {
        let mut groups = JoinSet::new();
        loop {
            tokio::select! {
                incoming = connection.accept_uni() => {
                    let mut incoming = match incoming { Ok(stream) => stream, Err(error) => return error };
                    if groups.len() >= 4 {
                        let _ = incoming.stop(RESET_GROUP.into());
                        self.counters.rejected.fetch_add(1, Ordering::Relaxed);
                        continue;
                    }
                    let streams = Arc::clone(&self);
                    groups.spawn(async move { let _ = timeout(LIFETIME, streams.group(conn, incoming)).await; });
                }
                _ = groups.join_next(), if !groups.is_empty() => {},
            }
        }
    }
    async fn group(&self, owner: Id, incoming: quinn::RecvStream) {
        let Ok(Ok((header, mut incoming))) = timeout(IDLE, read_header(incoming)).await else {
            self.counters.rejected.fetch_add(1, Ordering::Relaxed);
            return;
        };
        let cancel = Cancel::new();
        let mut guard = SourceGuard {
            cancel: cancel.clone(),
            complete: false,
        };
        let mut prefix = BytesMut::new();
        if header.encode_into(&mut prefix).is_err() {
            return;
        }
        let prefix = prefix.freeze();
        let targets = {
            let mut rooms = self.rooms.lock();
            let channel = rooms.seat(owner).map(|seat| seat.channel);
            let Some(track) = rooms
                .video
                .tracks
                .get_mut(&header.media.track_id)
                .filter(|track| track.owner == owner && Some(track.channel) == channel)
            else {
                let _ = incoming.stop(RESET_GROUP.into());
                self.counters.rejected.fetch_add(1, Ordering::Relaxed);
                return;
            };
            if track.groups.len() >= GROUP_BUDGET
                && let Some(old) = track.groups.pop_front()
            {
                old.stop();
            }
            track.groups.push_back(cancel.clone());
            track
                .viewers
                .values_mut()
                .map(|sub| {
                    let sub_cancel = Cancel::new();
                    if sub.groups.len() >= GROUP_BUDGET
                        && let Some(old) = sub.groups.pop_front()
                    {
                        old.stop();
                    }
                    sub.groups.push_back(sub_cancel.clone());
                    (sub.connection.clone(), sub_cancel)
                })
                .collect::<Vec<_>>()
        };
        let mut sinks = Vec::with_capacity(targets.len());
        for (connection, sub_cancel) in targets {
            let (tx, rx) = mpsc::channel(QUEUE_CHUNKS);
            tokio::spawn(downstream(
                connection,
                prefix.clone(),
                rx,
                cancel.clone(),
                sub_cancel.clone(),
                Arc::clone(&self.counters),
            ));
            sinks.push(Sink {
                tx,
                cancel: sub_cancel,
            });
        }
        let mut total = 0;
        loop {
            let next = tokio::select! {
                biased;
                () = cancel.cancelled() => { let _ = incoming.stop(RESET_GROUP.into()); return; },
                chunk = timeout(IDLE, incoming.read_chunk(CHUNK, true)) => chunk,
            };
            match next {
                Ok(Ok(Some(chunk))) => {
                    total += chunk.bytes.len();
                    if total > MAX_GROUP_BYTES {
                        let _ = incoming.stop(RESET_GROUP.into());
                        return;
                    }
                    sinks.retain(|sink| {
                        if sink.tx.try_send(chunk.bytes.clone()).is_ok() {
                            true
                        } else {
                            sink.cancel.stop();
                            false
                        }
                    });
                    // Ready source reads must not starve healthy downstream tasks.
                    tokio::task::yield_now().await;
                }
                Ok(Ok(None)) => {
                    guard.complete = true;
                    return;
                }
                _ => {
                    let _ = incoming.stop(RESET_GROUP.into());
                    return;
                }
            }
        }
    }
}
async fn downstream(
    connection: quinn::Connection,
    prefix: Bytes,
    mut chunks: mpsc::Receiver<Bytes>,
    source: Cancel,
    viewer: Cancel,
    counters: Arc<Counters>,
) {
    let cancelled = async {
        tokio::select! { () = source.cancelled() => {}, () = viewer.cancelled() => {} }
    };
    let transfer = async {
        let mut send = Output(connection.open_uni().await.map_err(|_| ())?);
        send.0.write_all(&prefix).await.map_err(|_| ())?;
        counters.bytes.fetch_add(
            u64::try_from(prefix.len()).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
        while let Some(chunk) = chunks.recv().await {
            send.0.write_all(&chunk).await.map_err(|_| ())?;
            counters.bytes.fetch_add(
                u64::try_from(chunk.len()).unwrap_or(u64::MAX),
                Ordering::Relaxed,
            );
        }
        send.0.finish().map_err(|_| ())?;
        send.0
            .stopped()
            .await
            .map_err(|_| ())?
            .map_or(Ok(()), |_| Err(()))
        // Output's reset is a no-op after a successful acknowledgement.
    };
    let result: Result<(), ()> =
        tokio::select! { biased; () = cancelled => Err(()), result = transfer => result };
    if result.is_err() {
        counters.resets.fetch_add(1, Ordering::Relaxed);
    }
}
