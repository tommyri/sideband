//! Per-second JSON lines: frame rate, bytes, barcode latency, gaps, and this
//! process's CPU time and memory footprint.

use std::time::Instant;

use serde_json::{Value, json};

use crate::stamp;

/// Process CPU seconds (user + system).
pub fn cpu_seconds() -> f64 {
    // SAFETY: getrusage fills a caller-owned struct.
    let usage = unsafe {
        let mut usage = std::mem::zeroed::<libc::rusage>();
        libc::getrusage(libc::RUSAGE_SELF, &mut usage);
        usage
    };
    let tv = |t: libc::timeval| t.tv_sec as f64 + t.tv_usec as f64 / 1e6;
    tv(usage.ru_utime) + tv(usage.ru_stime)
}

/// Physical footprint in MiB (what Activity Monitor calls Memory).
pub fn footprint_mib() -> f64 {
    // SAFETY: proc_pid_rusage fills a caller-owned rusage_info_v2.
    unsafe {
        let mut info = std::mem::zeroed::<libc::rusage_info_v2>();
        let rc = libc::proc_pid_rusage(
            libc::getpid(),
            libc::RUSAGE_INFO_V2,
            (&raw mut info).cast::<libc::rusage_info_t>(),
        );
        if rc == 0 {
            info.ri_phys_footprint as f64 / 1_048_576.0
        } else {
            -1.0
        }
    }
}

pub struct Window {
    role: String,
    started: Instant,
    last: Instant,
    cpu_last: f64,
    frames: u64,
    bytes: u64,
    stamped: u64,
    latencies: Vec<f64>,
    last_frame: Option<Instant>,
    max_gap_ms: f64,
    last_seq: Option<u16>,
    skipped: u64,
    size: (u32, u32),
    pub extra: serde_json::Map<String, Value>,
}

impl Window {
    pub fn new(role: &str) -> Self {
        let now = Instant::now();
        Self {
            role: role.to_owned(),
            started: now,
            last: now,
            cpu_last: cpu_seconds(),
            frames: 0,
            bytes: 0,
            stamped: 0,
            latencies: Vec::new(),
            last_frame: None,
            max_gap_ms: 0.0,
            last_seq: None,
            skipped: 0,
            size: (0, 0),
            extra: serde_json::Map::new(),
        }
    }

    pub fn bytes(&mut self, n: usize) {
        self.bytes += n as u64;
    }

    /// One frame delivered (sent or decoded). `barcode` is the decoded stamp, if any.
    pub fn frame(&mut self, size: (u32, u32), barcode: Option<(u64, u16)>) {
        let now = Instant::now();
        if let Some(prev) = self.last_frame {
            self.max_gap_ms = self
                .max_gap_ms
                .max(now.duration_since(prev).as_secs_f64() * 1e3);
        }
        self.last_frame = Some(now);
        self.frames += 1;
        self.size = size;
        if let Some((micros, seq)) = barcode {
            self.stamped += 1;
            let latency = stamp::now_micros().saturating_sub(micros) as f64 / 1e3;
            self.latencies.push(latency);
            if let Some(last) = self.last_seq {
                let step = seq.wrapping_sub(last);
                if step > 1 && step < 30_000 {
                    self.skipped += u64::from(step - 1);
                }
            }
            self.last_seq = Some(seq);
        }
    }

    /// Emit a line when a second has passed. Returns true when it printed.
    pub fn tick(&mut self) -> bool {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last).as_secs_f64();
        if elapsed < 1.0 {
            return false;
        }
        // Count a stall in progress as a gap too.
        if let Some(prev) = self.last_frame {
            self.max_gap_ms = self
                .max_gap_ms
                .max(now.duration_since(prev).as_secs_f64() * 1e3);
        } else {
            self.max_gap_ms = now.duration_since(self.started).as_secs_f64() * 1e3;
        }
        let cpu = cpu_seconds();
        self.latencies.sort_by(f64::total_cmp);
        let pct = |p: f64| -> Value {
            if self.latencies.is_empty() {
                Value::Null
            } else {
                let i = ((self.latencies.len() - 1) as f64 * p).round() as usize;
                json!((self.latencies[i] * 10.0).round() / 10.0)
            }
        };
        let mut line = json!({
            "t_ms": stamp::now_micros() / 1000,
            "since_s": (now.duration_since(self.started).as_secs_f64() * 10.0).round() / 10.0,
            "role": self.role,
            "fps": (self.frames as f64 / elapsed * 10.0).round() / 10.0,
            "kbps": (self.bytes as f64 * 8.0 / elapsed / 1000.0).round(),
            "w": self.size.0,
            "h": self.size.1,
            "stamped": self.stamped,
            "skipped": self.skipped,
            "lat_p50_ms": pct(0.5),
            "lat_p95_ms": pct(0.95),
            "lat_max_ms": pct(1.0),
            "max_gap_ms": self.max_gap_ms.round(),
            "cpu_pct": ((cpu - self.cpu_last) / elapsed * 1000.0).round() / 10.0,
            "mem_mib": (footprint_mib() * 10.0).round() / 10.0,
        });
        if let Value::Object(map) = &mut line {
            map.extend(std::mem::take(&mut self.extra));
        }
        println!("{line}");
        self.last = now;
        self.cpu_last = cpu;
        self.frames = 0;
        self.bytes = 0;
        self.stamped = 0;
        self.skipped = 0;
        self.latencies.clear();
        self.max_gap_ms = 0.0;
        true
    }
}

/// One event line (phase change, reconnect, revocation...).
pub fn event(role: &str, name: &str, detail: Value) {
    println!(
        "{}",
        json!({"t_ms": stamp::now_micros() / 1000, "role": role, "event": name, "detail": detail})
    );
}
