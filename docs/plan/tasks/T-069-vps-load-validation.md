# T-069 — Load validation on the VPS and UDP buffer tuning

Status: todo
Phase: 4
Size: S
Depends on: T-062, T-034
Paths: `docs/plan/notes/load-test.md`, `crates/ss-net/src/server.rs`, `crates/ss-server/src/`, `deploy/README.md`

## Goal
Verify the T-062 acceptance criteria on the real VPS. T-062 only ran
shortened scenarios on a development Mac.

## Scope
- Run from a second machine against the VPS, following the "To run on the
  VPS" section of `docs/plan/notes/load-test.md`:
  - 2-hour soak, 25 users, 3 voice channels;
  - 15-minute headroom run, 100 users, 10 channels, with `perf` to confirm
    or refute that UDP send syscalls on the one runtime thread are the
    bottleneck (about 60 % of busy time on macOS);
  - 5-minute all-talking stress with `SS_WORKER_THREADS` 1 and 2;
  - 15-minute storm run, 100 users, a storm every 120 s.
- Record `netstat -su` `RcvbufErrors` and `SndbufErrors` before and after
  every run. If either grows, request larger socket buffers on the server
  endpoint through quinn-udp (`set_recv_buffer_size`,
  `set_send_buffer_size`) with a config knob, and re-run; today the socket
  keeps the kernel default of 208 KiB on Linux, so the `rmem_max` sysctl in
  `deploy/README.md` only raises a ceiling nobody asks for.
- Settle the RSS question: the 30-minute local soak extrapolated to about
  1.8 MiB/h, which would just fail the 10 % bar over two hours. Decide leak
  or high-water mark from the VPS run; a leak becomes a fix in this task if
  small, otherwise a new task.
- If registering 100 bots at five logins per minute blocks the runs, add a
  server-side seed command (`ss-server user seed --count N --prefix load-`)
  that creates accounts without the login limiter, owner-only, documented
  in the server README.
- Update `docs/plan/notes/load-test.md` with the numbers and graphs, and
  `deploy/README.md` with any tuning that changed.

## Out of scope
Horizontal scaling; multi-server identity.

## Acceptance criteria
- 25-user soak on the VPS: RSS growth ≤ 10 % after a 5-minute warm-up, zero
  forced disconnects, < 0.1 % datagram drops.
- 100-user numbers recorded with the bottleneck confirmed on Linux.
- Any threshold that fails becomes a task with the measurement attached.

## Notes from implementation
