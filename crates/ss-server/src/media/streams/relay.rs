//! Opaque streaming fan-out: bounded chunks per downstream group, never wait
//! for a viewer on the sender's read path. FIN remains cancellable until ACK.
use super::{
    Cancel, Counters, GROUP_BUDGET, Group, Id, Ordering, RenditionCounters, Streams,
    adapt::{self, Estimator, Forward, Link, PROBE_PRIORITY, TICK},
    report_low_change,
};
use bytes::{Bytes, BytesMut};
use ss_net::media::stream::{RESET_GROUP, read_header};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{sync::mpsc, task::JoinSet, time::timeout};

const CHUNK: usize = 16 * 1024;
/// Per viewer and group: room for a 1080p keyframe burst. Chunks are shared
/// `Bytes`, so only a viewer that falls behind holds memory here.
const QUEUE_CHUNKS: usize = 32;
const MAX_GROUP_BYTES: usize = 32 * 1024 * 1024;
const IDLE: Duration = Duration::from_secs(5);
const LIFETIME: Duration = Duration::from_secs(30);
/// Incoming group tasks per connection: two renditions, each with a group
/// finishing while the next starts, plus forced keyframes.
const INCOMING_GROUPS: usize = 8;
struct Sink {
    tx: mpsc::Sender<Bytes>,
    cancel: Cancel,
    link: Arc<Link>,
}
struct SourceGuard {
    group: Group,
    complete: bool,
}
impl Drop for SourceGuard {
    fn drop(&mut self) {
        if self.complete {
            self.group.done.store(true, Ordering::Relaxed);
        } else {
            self.group.cancel.stop();
        }
    }
}
struct Output(quinn::SendStream);
impl Drop for Output {
    fn drop(&mut self) {
        let _ = self.0.reset(RESET_GROUP.into());
    }
}
/// Everything one downstream transfer reports into.
struct Downstream {
    connection: quinn::Connection,
    prefix: Bytes,
    source: Cancel,
    viewer: Group,
    priority: i32,
    rendition: usize,
    counters: Arc<Counters>,
    track: Arc<RenditionCounters>,
    link: Arc<Link>,
}

/// Retain `group` in a per-rendition budget, cancelling the oldest. Returns
/// true when the evicted group was still transferring.
fn retain(groups: &mut std::collections::VecDeque<Group>, group: Group) -> bool {
    let mut evicted_live = false;
    if groups.len() >= GROUP_BUDGET
        && let Some(old) = groups.pop_front()
    {
        evicted_live = old.live();
        old.cancel.stop();
    }
    groups.push_back(group);
    evicted_live
}

impl Streams {
    pub(crate) async fn run(
        self: Arc<Self>,
        conn: Id,
        connection: quinn::Connection,
    ) -> quinn::ConnectionError {
        let mut groups = JoinSet::new();
        let mut estimator = Estimator::default();
        let mut tick = tokio::time::interval(TICK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut ticks = 0_u32;
        loop {
            tokio::select! {
                incoming = connection.accept_uni() => {
                    let mut incoming = match incoming { Ok(stream) => stream, Err(error) => return error };
                    if groups.len() >= INCOMING_GROUPS {
                        let _ = incoming.stop(RESET_GROUP.into());
                        self.counters.rejected.fetch_add(1, Ordering::Relaxed);
                        continue;
                    }
                    let streams = Arc::clone(&self);
                    groups.spawn(async move { let _ = timeout(LIFETIME, streams.group(conn, incoming)).await; });
                }
                _ = groups.join_next(), if !groups.is_empty() => {},
                _ = tick.tick() => {
                    ticks = ticks.wrapping_add(1);
                    self.tick(conn, &connection, &mut estimator, ticks);
                }
            }
        }
    }
    #[allow(clippy::too_many_lines)]
    async fn group(&self, owner: Id, incoming: quinn::RecvStream) {
        let Ok(Ok((header, mut incoming))) = timeout(IDLE, read_header(incoming)).await else {
            self.counters.rejected.fetch_add(1, Ordering::Relaxed);
            return;
        };
        let source = Group::new();
        let cancel = source.cancel.clone();
        let mut guard = SourceGuard {
            group: source.clone(),
            complete: false,
        };
        let mut prefix = BytesMut::new();
        if header.encode_into(&mut prefix).is_err() {
            return;
        }
        let prefix = prefix.freeze();
        let rendition = header.rendition();
        let index = usize::from(rendition);
        let targets = {
            let mut rooms = self.rooms.write();
            let channel = rooms.seat(owner).map(|seat| seat.channel);
            let state = &mut rooms.video;
            let Some(track) = state
                .tracks
                .get_mut(&header.media.track_id)
                .filter(|track| track.owner == owner && Some(track.channel) == channel)
                .filter(|track| index < track.renditions())
            else {
                let _ = incoming.stop(RESET_GROUP.into());
                self.counters.rejected.fetch_add(1, Ordering::Relaxed);
                return;
            };
            // A finished incoming group stays finished: only a live one is
            // stopped, so its downstream tails are never reset from here.
            if track.groups[index].len() >= GROUP_BUDGET
                && let Some(old) = track.groups[index].pop_front()
                && old.live()
            {
                old.cancel.stop();
            }
            track.groups[index].push_back(source.clone());
            let renditions = track.renditions();
            let counters = Arc::clone(&track.counters);
            let now = Instant::now();
            let mut moved = false;
            let mut targets = Vec::new();
            for sub in track.viewers.values_mut() {
                let decision =
                    sub.select
                        .on_group(rendition, renditions, sub.link.congested(), now);
                let priority = match decision {
                    Forward::Skip => continue,
                    Forward::Normal => 0,
                    Forward::Probe => PROBE_PRIORITY,
                    Forward::Up => {
                        tracing::info!(
                            track = header.media.track_id,
                            "video viewer moved back to the full rendition"
                        );
                        adapt::up(sub, &self.counters);
                        moved = true;
                        0
                    }
                };
                let viewer = Group::new();
                if retain(&mut sub.groups[index], viewer.clone()) {
                    sub.link.resets.fetch_add(1, Ordering::Relaxed);
                    counters.resets[index].fetch_add(1, Ordering::Relaxed);
                }
                targets.push((
                    sub.connection.clone(),
                    viewer,
                    priority,
                    Arc::clone(&sub.link),
                ));
            }
            if moved {
                report_low_change(&state.registry, track);
            }
            targets
                .into_iter()
                .map(|(connection, viewer, priority, link)| {
                    let (tx, rx) = mpsc::channel(QUEUE_CHUNKS);
                    let sink = Sink {
                        tx,
                        cancel: viewer.cancel.clone(),
                        link: Arc::clone(&link),
                    };
                    let downstream = Downstream {
                        connection,
                        prefix: prefix.clone(),
                        source: cancel.clone(),
                        viewer,
                        priority,
                        rendition: index,
                        counters: Arc::clone(&self.counters),
                        track: Arc::clone(&counters),
                        link,
                    };
                    (sink, rx, downstream)
                })
                .collect::<Vec<_>>()
        };
        let mut sinks = Vec::with_capacity(targets.len());
        let mut track_counters = None;
        for (sink, rx, downstream) in targets {
            track_counters.get_or_insert_with(|| Arc::clone(&downstream.track));
            tokio::spawn(downstream.run(rx));
            sinks.push(sink);
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
                            // This viewer fell behind: reset its group, count it.
                            if !sink.tx.is_closed() {
                                sink.link.resets.fetch_add(1, Ordering::Relaxed);
                                if let Some(track) = &track_counters {
                                    track.resets[index].fetch_add(1, Ordering::Relaxed);
                                }
                            }
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

impl Downstream {
    async fn run(self, mut chunks: mpsc::Receiver<Bytes>) {
        let Self {
            connection,
            prefix,
            source,
            viewer,
            priority,
            rendition,
            counters,
            track,
            link,
        } = self;
        let cancelled = async {
            tokio::select! { () = source.cancelled() => {}, () = viewer.cancel.cancelled() => {} }
        };
        let count = |bytes: usize| {
            let bytes = u64::try_from(bytes).unwrap_or(u64::MAX);
            counters.bytes.fetch_add(bytes, Ordering::Relaxed);
            counters.rendition_bytes[rendition].fetch_add(bytes, Ordering::Relaxed);
            track.bytes[rendition].fetch_add(bytes, Ordering::Relaxed);
            link.written.fetch_add(bytes, Ordering::Relaxed);
        };
        let transfer = async {
            let mut send = Output(connection.open_uni().await.map_err(|_| ())?);
            if priority != 0 {
                let _ = send.0.set_priority(priority);
            }
            send.0.write_all(&prefix).await.map_err(|_| ())?;
            count(prefix.len());
            while let Some(chunk) = chunks.recv().await {
                send.0.write_all(&chunk).await.map_err(|_| ())?;
                count(chunk.len());
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
        viewer.done.store(true, Ordering::Relaxed);
        if result.is_err() {
            counters.resets.fetch_add(1, Ordering::Relaxed);
        }
    }
}
