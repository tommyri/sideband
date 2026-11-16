# T-117 — Real-time voice priority on Linux through rtkit

Status: awaiting-acceptance
Phase: 3
Size: S
Depends on: T-083, T-106
Paths: `crates/ss-audio/Cargo.toml`, `crates/ss-client-core/src/voice/`, `Cargo.toml`, `deny.toml`, `scripts/install-linux-deps.sh`, `docs/dependency-maintenance.md`

## Goal
The voice thread runs at real-time priority on Linux, as it already does on
macOS and Windows.

Source: T-106. The maintainer accepted rtkit over D-Bus on 2026-11-12.
T-083 added `audio_thread_priority`; on Linux promotion needs its
`with_dbus` feature, so the voice worker runs at normal priority there today.

## Scope
- Enable `audio_thread_priority`'s D-Bus path on Linux only, and check what
  it pulls in (`dbus`, `libdbus-sys`) against `deny.toml` and the
  dependency policy. Record the licences.
- Add the build dependency (`libdbus-1-dev` or the distribution's
  equivalent) to `scripts/install-linux-deps.sh` and the Linux CI job.
- When rtkit is missing or refuses (no session bus, a container, limits
  too low), keep running at normal priority and say so once in the log and
  in diagnostics, as T-083 does for other failures.

## Out of scope
macOS and Windows priority. Changing the audio callback threads.

## Acceptance criteria
- `cargo deny --all-features check` passes with the new dependencies.
- The Linux CI build compiles the D-Bus path.
- Manual on Linux: during a call, `chrt -p` on the voice thread shows a
  real-time policy, and with rtkit stopped the call still works and the log
  says why.

## Notes from implementation

### Findings
- `audio_thread_priority` 0.35 with `with_dbus` asks rtkit over the
  *system* bus: it sets `RLIMIT_RTTIME` (soft = one audio period, hard =
  rtkit's `RTTimeUSecMax`, 200 ms by default), then calls
  `MakeThreadRealtime` (`SCHED_RR`, priority 10). The D-Bus call blocks for
  at most 10 s.
- The soft limit is process-wide and the default action of `SIGXCPU` ends
  the process. A real-time thread that runs one period (10 ms for the
  worker) without blocking would therefore kill the client. Checked in a
  privileged container with rtkit running: a promoted thread spinning 60 ms
  ended the test process with signal 24 (`SIGXCPU`) when the guard below was
  removed, and passed with it.
- Features unify: `with_dbus` also turns on rtkit for `cpal`'s own Linux
  threads (PipeWire via its background promoter, ALSA at stream start),
  which T-083 found were never promoted. They also reset the soft limit, so
  raising our own budget would not have been enough.
- rtkit is D-Bus activated. `systemctl stop rtkit-daemon` alone does not
  keep it away: the next request starts it again. Mask it first.

### What changed
- `crates/ss-audio/Cargo.toml`: on Linux only,
  `audio_thread_priority = { workspace = true, features = ["with_dbus"] }`
  and `signal-hook`. macOS and Windows dependency trees are unchanged
  (`cargo tree --target` for `aarch64-apple-darwin` and
  `x86_64-pc-windows-msvc` shows no `dbus`, `libdbus-sys` or `signal-hook`).
- `Cargo.toml`: `signal-hook = { version = "0.3", default-features = false }`,
  comment on `audio_thread_priority`. `Cargo.lock`: `dbus` 0.6.5 added;
  `ss-audio` gains `signal-hook`.
- `ss-audio/src/device/stream.rs`: `promote_audio_thread` no longer returns
  an error on Linux before trying; a refusal reads `rtkit: <D-Bus error>`.
  `guard_real_time_limit` registers a `SIGXCPU` handler once per process
  (Linux only, no-op elsewhere), from `promote_audio_thread` and from
  `DeviceManager::new` (before cpal starts any stream thread). The hard
  limit still stops a thread that really runs away.
- `ss-client-core/src/voice/worker.rs`: the promotion call is a seam
  (`Runtime::promote`, `device::promote_audio_thread` in builds).
  `promote_thread` runs once at the start of `Runtime::run`; on a refusal it
  logs `voice worker keeps normal priority` at info with the reason, keeps
  the reason, and the loop goes on. `WorkerStats.realtime_error` carries it.
- `ss-client-core/src/voice/mod.rs`: diagnostics gain
  `details.workerRealtimeError` (string or null) beside `workerRealtime`.
- `scripts/install-linux-deps.sh`: `libdbus-1-dev` in the `desktop` and
  `audio` sets. The Linux CI job, container smoke (`ss-cli` on the host) and
  reconnect chaos (`ss-server` tests through `ss-client-core`) install those
  sets, so `ci.yml` needs no edit. The server image builds `ss-server` only
  and does not link `ss-audio`.

### Dependencies and licences
- `dbus` 0.6.5: MIT OR Apache-2.0 (`Apache-2.0/MIT`). New. Required by
  `audio_thread_priority` 0.35 (`^0.6.4`), so it sits beside tao's 0.9.12:
  a `multiple-versions` warning, which the policy allows.
- `libdbus-sys` 0.2.7: MIT OR Apache-2.0. Already in the lockfile (tao).
  Build: `pkg-config` and `dbus-1 >= 1.6` (`libdbus-1-dev`); not vendored.
  Links the system `libdbus-1.so.3` (AFL-2.1 OR GPL-2.0-or-later, dynamic,
  already linked by the Linux desktop through tao; `ss-cli` now links it
  too).
- `signal-hook` 0.3.18 and `signal-hook-registry` 1.4.8: MIT OR
  Apache-2.0. Already in the lockfile (crossterm, `ss-cli`).
- `audio_thread_priority` stays MPL-2.0 under its existing `deny.toml`
  exception; the comment there records the D-Bus crates.
- `cargo deny --all-features check`: advisories, bans, licences and sources
  ok.

### Tests
- Written first: `voice::worker::tests::a_refused_promotion_is_reported_once_and_the_call_goes_on`.
  It runs `Runtime::run` for 300 ms with a promoter that refuses with
  `rtkit: no system bus`: one promotion attempt, at least 15 quanta still
  captured, `realtime` false and `realtime_error` the reason in the
  published stats. It failed on the missing reason before the change. The
  log line sits in the same once-only function; this crate has no log
  capture, so the test counts the attempt rather than the line.
- `device::stream::tests::this_platform_accepts_a_real_time_audio_thread`:
  on Linux, a refusal must start with `rtkit: ` (the outcome depends on the
  machine). New `a_real_time_thread_may_run_past_its_period` (Linux,
  `#[ignore]`, needs a system bus and rtkit): a promoted thread spins 60 ms
  past a 10 ms period and the process survives.

### What was compiled for Linux, and how
- On this Mac, `cargo check --target x86_64-unknown-linux-gnu -p ss-audio`
  stops in build scripts with no Linux sysroot: `libdbus-sys` 0.2.7 and
  `alsa-sys` 0.4.0 (`pkg-config` has no cross configuration).
- In throwaway `rust:1.99.1-bookworm` containers (arm64, native; worktree
  mounted read-only, separate target volume, removed afterwards), with the
  `audio` packages from `install-linux-deps.sh`:
  - Without `libdbus-1-dev`, `cargo check -p ss-audio` fails at
    `libdbus-sys` (`dbus-1` not found), which is why the script needs it.
  - With it (dbus 1.14.10): `cargo check -p ss-audio -p ss-client-core
    --all-targets` and `cargo clippy -p ss-audio -p ss-client-core -p ss-cli
    --all-targets -- -D warnings` pass. `cargo tree -i audio_thread_priority
    -e features` shows `with_dbus`; the `ss-audio` test binary links
    `libdbus-1.so.3`. `cargo test -p ss-audio --lib device::stream` and
    `cargo test -p ss-client-core --lib voice` pass.
  - The real refusal texts: no system bus,
    `rtkit: AudioThreadPriorityError: org.freedesktop.DBus.Error.FileNotFound:Failed to connect to socket /run/dbus/system_bus_socket: No such file or directory`;
    a bus without rtkit, `...DBus.Error.ServiceUnknown:The name org.freedesktop.RealtimeKit1 was not provided by any .service files`.
  - Privileged container with `dbus-daemon --system`, polkitd and
    `rtkit-daemon`: promotion returned `Ok(())`, and the ignored overrun test
    passed (and died with `SIGXCPU` without the guard, above). Without
    `--privileged`, rtkit itself could not start (`cap_set_proc() failed`).
  - The desktop crate was not built for Linux (WebKitGTK stack); it adds no
    code on this path. GitHub's Linux job has not been dispatched.

### Deviations
- `signal-hook` and the `SIGXCPU` guard are not in the task's scope. Without
  them the change would let a 10 ms overrun of any real-time thread in the
  process end the client.
- Promotion stays synchronous at the start of `Runtime::run`, after the
  worker has reported ready. A hung system bus could hold the first audio
  for up to the crate's 10 s D-Bus timeout; an absent bus, an absent or
  masked rtkit, or a refusal answers at once.
- The resampler threads also go through rtkit now; their refusal stays a
  debug line per thread, as before.

### Files outside the Paths
- `crates/ss-audio/src/device/stream.rs` (also changed by T-118):
  `promote_audio_thread`, the new `guard_real_time_limit` just below it, and
  the two priority tests. No other hunk.
- `crates/ss-audio/src/device/manager.rs`: one call in `DeviceManager::new`.
- `Cargo.lock`.
- `docs/design/voice.md`: the real-time paragraph and the diagnostics line.
- `docs/plan/tasks/INDEX.md` and `ACCEPTANCE.md`: status and rows.

### For hands-on testing (Linux desktop with PipeWire and rtkit)
1. Build and run the desktop (or `ss-cli`) on Linux; `libdbus-1-dev` must
   be installed. Check `systemctl status rtkit-daemon` (or that
   `busctl --system status org.freedesktop.RealtimeKit1` answers).
2. Join a voice call. Find the voice thread in any client process:
   `ps -eLo pid,tid,cls,rtprio,comm | grep -E 'ss-client-voice|ss-audio-'`.
   Then `chrt -p <tid>` on the `ss-client-voice` line: expect `SCHED_RR`
   with priority 10 (`ps` shows `RR 10`). The resampler threads
   (`ss-audio-captur`, `ss-audio-playba`; only when a device is not at
   48 kHz) should also show `RR`.
3. Diagnostics (stats panel or `details`): `workerRealtime` true,
   `workerRealtimeError` null. `journalctl -u rtkit-daemon` shows
   "Successfully made thread … of process … RT at priority 10".
4. Leave the call. Stop rtkit so D-Bus cannot start it again:
   `sudo systemctl mask --now rtkit-daemon` (on some distributions
   `rtkit-daemon.service` is named `rtkit.service`).
5. Join again. The call must work normally. The log has one line
   `voice worker keeps normal priority` with an `error` field starting
   `rtkit:` (for a masked unit, a D-Bus error naming the masked service or
   `ServiceUnknown`). `workerRealtime` false, `workerRealtimeError` the same
   text. `chrt -p <tid>` shows `SCHED_OTHER`.
6. Restore: `sudo systemctl unmask rtkit-daemon` and start it again.
7. Optional: run `cargo test -p ss-audio --lib a_real_time_thread_may_run_past_its_period -- --ignored`
   with rtkit running; it must pass.
