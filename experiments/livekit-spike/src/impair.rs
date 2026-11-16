//! A user-space UDP impairment proxy: rate limit with a bounded queue, random
//! loss and added delay, on a timed schedule. No root, no system changes.
//! Downstream (server to client) gets the rate limit; both directions get the
//! loss and delay.

use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use rand::Rng;
use serde_json::json;
use tokio::{net::UdpSocket, sync::Mutex};

use crate::metrics;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Impairment {
    pub kbps: Option<u64>,
    pub loss: f64,
    pub delay_ms: u64,
    pub max_queue_ms: u64,
}

/// `20:clean,30:1500kbps/2%/40ms,40:clean`: seconds per phase, then the shape.
pub fn parse_schedule(spec: &str) -> Result<Vec<(u64, Option<Impairment>)>> {
    spec.split(',')
        .map(|phase| {
            let (secs, shape) = phase.split_once(':').context("phase is <secs>:<shape>")?;
            let secs = secs.parse()?;
            if shape == "clean" {
                return Ok((secs, None));
            }
            let mut imp = Impairment {
                kbps: None,
                loss: 0.0,
                delay_ms: 0,
                max_queue_ms: 200,
            };
            for part in shape.split('/') {
                if let Some(v) = part.strip_suffix("kbps") {
                    imp.kbps = Some(v.parse()?);
                } else if let Some(v) = part.strip_suffix('%') {
                    imp.loss = v.parse::<f64>()? / 100.0;
                } else if let Some(v) = part.strip_suffix("ms") {
                    imp.delay_ms = v.parse()?;
                } else if let Some(v) = part.strip_prefix("q") {
                    imp.max_queue_ms = v.parse()?;
                } else {
                    bail!("unknown impairment {part}");
                }
            }
            Ok((secs, Some(imp)))
        })
        .collect()
}

struct Shaper {
    next_free: Instant,
}

struct State {
    current: Option<Impairment>,
    dropped_loss: u64,
    dropped_queue: u64,
    forwarded: u64,
    bytes_down: u64,
}

pub async fn run(
    listen: SocketAddr,
    upstream: SocketAddr,
    schedule: Vec<(u64, Option<Impairment>)>,
) -> Result<()> {
    let front = Arc::new(UdpSocket::bind(listen).await?);
    let state = Arc::new(Mutex::new(State {
        current: None,
        dropped_loss: 0,
        dropped_queue: 0,
        forwarded: 0,
        bytes_down: 0,
    }));
    metrics::event(
        "impair",
        "listening",
        json!({"listen": listen.to_string(), "upstream": upstream.to_string()}),
    );
    {
        let state = Arc::clone(&state);
        tokio::spawn(async move {
            for (secs, phase) in schedule {
                state.lock().await.current = phase;
                metrics::event(
                    "impair",
                    "phase",
                    json!({"impairment": phase.map(|p| json!({"kbps": p.kbps, "loss": p.loss, "delay_ms": p.delay_ms, "max_queue_ms": p.max_queue_ms})), "secs": secs}),
                );
                tokio::time::sleep(Duration::from_secs(secs)).await;
            }
            state.lock().await.current = None;
            metrics::event(
                "impair",
                "phase",
                json!({"impairment": null, "secs": "rest"}),
            );
        });
    }
    {
        let state = Arc::clone(&state);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(1)).await;
                let mut s = state.lock().await;
                println!(
                    "{}",
                    json!({"t_ms": crate::stamp::now_micros() / 1000, "role": "impair", "fwd": s.forwarded, "drop_loss": s.dropped_loss, "drop_queue": s.dropped_queue, "down_kbps": s.bytes_down * 8 / 1000})
                );
                s.bytes_down = 0;
            }
        });
    }
    let mut clients: HashMap<SocketAddr, Arc<UdpSocket>> = HashMap::new();
    let mut buf = vec![0_u8; 65_536];
    loop {
        let (n, from) = front.recv_from(&mut buf).await?;
        let back = match clients.get(&from) {
            Some(s) => Arc::clone(s),
            None => {
                let back = Arc::new(UdpSocket::bind("127.0.0.1:0").await?);
                back.connect(upstream).await?;
                clients.insert(from, Arc::clone(&back));
                metrics::event("impair", "client", json!({"from": from.to_string()}));
                tokio::spawn(downstream(
                    Arc::clone(&back),
                    Arc::clone(&front),
                    from,
                    Arc::clone(&state),
                ));
                back
            }
        };
        let packet = buf[..n].to_vec();
        let current = state.lock().await.current;
        if let Some(imp) = current {
            if rand::rng().random::<f64>() < imp.loss {
                state.lock().await.dropped_loss += 1;
                continue;
            }
            if imp.delay_ms > 0 {
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(imp.delay_ms)).await;
                    let _ = back.send(&packet).await;
                });
                continue;
            }
        }
        let _ = back.send(&packet).await;
    }
}

async fn downstream(
    back: Arc<UdpSocket>,
    front: Arc<UdpSocket>,
    client: SocketAddr,
    state: Arc<Mutex<State>>,
) {
    let mut buf = vec![0_u8; 65_536];
    let mut shaper = Shaper {
        next_free: Instant::now(),
    };
    while let Ok(n) = back.recv(&mut buf).await {
        let packet = buf[..n].to_vec();
        let now = Instant::now();
        let current = state.lock().await.current;
        let mut send_at = now;
        if let Some(imp) = current {
            if rand::rng().random::<f64>() < imp.loss {
                state.lock().await.dropped_loss += 1;
                continue;
            }
            if let Some(kbps) = imp.kbps {
                let start = shaper.next_free.max(now);
                if start.duration_since(now) > Duration::from_millis(imp.max_queue_ms) {
                    state.lock().await.dropped_queue += 1;
                    continue;
                }
                let tx = Duration::from_secs_f64(n as f64 * 8.0 / (kbps as f64 * 1000.0));
                shaper.next_free = start + tx;
                send_at = shaper.next_free;
            }
            send_at += Duration::from_millis(imp.delay_ms);
        } else {
            shaper.next_free = now;
        }
        {
            let mut s = state.lock().await;
            s.forwarded += 1;
            s.bytes_down += n as u64;
        }
        let front = Arc::clone(&front);
        if send_at <= now {
            let _ = front.send_to(&packet, client).await;
        } else {
            tokio::spawn(async move {
                tokio::time::sleep_until(send_at.into()).await;
                let _ = front.send_to(&packet, client).await;
            });
        }
    }
}
