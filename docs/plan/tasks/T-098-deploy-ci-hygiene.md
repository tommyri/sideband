# T-098 — Deploy and CI hygiene: fail-closed checks, compose and NGINX limits, backup ownership

Status: awaiting-acceptance
Phase: 4
Size: M
Depends on: T-034, T-001
Paths: `scripts/check-deps.sh`, `scripts/check-public-repo.py`, `apps/desktop/.prettierignore`, `.github/workflows/`, `deploy/`, `deny.toml`, `.cargo/config.toml`, `docs/dependency-maintenance.md`, `README.md`

## Goal
The checks fail when something is wrong, the sample deployment is safe as
shipped, and the documents describe what the files do.

Source: 2026-11-09 review at `d861a0a`. Read from the files; the VPS items
are untested and need a Linux host.

## Scope
- `scripts/check-deps.sh` tests `if deps_of … | grep -qx`, so any
  `cargo tree` failure makes every check pass and prints "dependency graph
  ok". Capture the output first so `set -e` applies, then grep it.
- `pnpm check` runs Prettier over `pnpm-lock.yaml`. With a pnpm other than
  the pinned 12.18.0 the lockfile is rewritten in pnpm's own format and
  the check then fails on it (seen 2026-11-09 with pnpm 11.9). Add the
  lockfile to `.prettierignore` and commit it in pnpm's format; make
  `just check-desktop` fail early with a clear message on the wrong pnpm or
  Node version.
- `check-public-repo.py` matches the exact name `pins.json`, while the CLI
  writes `socialspeak-pins.json`; `.wav` and `.zip` are checked at the root
  only. Match `*pins.json` and check all directories.
- Decision for the maintainer, record the answer here: all workflows are
  manual-only, so pushes get no signal, Windows and Linux breakage is
  invisible from a Mac, and the secret scan runs only when remembered.
  Options: keep manual-only; run the cheap Linux `just check` and the
  public-repo check on push to `main`; or run the full matrix on pull
  requests. `04-conventions.md` still says CI runs on three systems; align
  it with the decision.
  **Decision (maintainer, 2026-11-11): keep manual-only.** No push or
  pull-request trigger is added. `04-conventions.md` and the README now say
  that workflows run only when dispatched, that local `just check` and
  `just check-desktop` are the routine gate, and which workflows to dispatch
  before a merge or a release. Options weighed (the repository is public, so
  standard hosted runners cost no minutes; the cost is queue time and noise):
  manual-only keeps every run deliberate but leaves Windows/Linux breakage
  and leaked secrets unseen until someone dispatches; Linux `just check`
  plus the public-repo check on push to `main` (roughly 10 to 25 minutes per
  push with a warm cache) catches Linux and secret regressions after the
  fact; the full matrix on pull requests catches everything before merge
  but runs macOS and Windows on every push to a PR. The implementer
  recommended the push-to-`main` option; no trigger change was prepared.
- Features never compiled in CI: `ss-audio` `aec` alone, `ss-video`
  `vaapi`, `nvidia` and `--no-default-features`. Add them or note why not.
  `deny.toml` has no `all-features`; confirm with
  `cargo deny --all-features check` whether optional dependencies are
  skipped.
- Compose: no `cap_drop: [ALL]`, `read_only`, `no-new-privileges` or
  healthcheck; `cap_add: NET_BIND_SERVICE` is redundant.
  `SS_INTERNAL_BIND=0.0.0.0:9101` makes the documented `ss-server conn
  list` refuse to run in the container. Add the hardening keys and fix the
  Grafana README.
- NGINX sample: every client shares one upstream address, so the server's
  per-address HTTP limits collapse into one bucket and one looping client
  gets everyone a 429. Add `limit_req_zone`/`limit_conn_zone` on
  `$binary_remote_addr` and an upstream keepalive block.
- `renew-sideband-certificate.sh` runs `nginx -t` before installing the
  bundle, so an unrelated broken site blocks the QUIC certificate update.
  Install first, then test and reload. Check `openssl x509 -checkhost`
  exit status on the target OpenSSL 3.0.x.
- `smoke.sh` reuses named volumes across runs and leaves its log; neither
  compose file is exercised by CI.
- Untested, high confidence: the backup command in `deploy/README.md` uses
  a transient unit with `DynamicUser=yes` and no `User=`, which may chown
  the state directory away from the running server; restore with `sudo tar`
  leaves the database owned by root. Check with `stat` on a VPS before and
  after each step and fix the commands.
- Document drift: T-034 notes say dispatch does not publish and that
  aarch64 uses a cross compiler; `.cargo/config.toml` still names it;
  `dependency-maintenance.md` claims job timeouts that two workflows lack;
  the README points to `test-desktop.yml` for the Linux package list that
  now lives in `scripts/install-linux-deps.sh`; some jobs use `*-latest`
  runners while the rest are pinned; the `deepfilter` feature is an empty
  stub that `04-conventions.md` lists as real.

## Out of scope
Release gating and signing (T-097). File-descriptor limits (T-092).

## Acceptance criteria
- `check-deps.sh` exits non-zero when `cargo tree` fails (test by naming a
  missing crate).
- `pnpm check` leaves `git status` clean with the pinned toolchain and
  fails with a clear message otherwise.
- Backup and restore run end to end on a VPS with ownership recorded.
- The CI decision is written in `04-conventions.md` and the README.

## Notes from implementation

Implemented 2026-11-11 on `task/T-098-deploy-ci-hygiene`. Status stays
`in-progress` until the VPS backup/restore run and the lockfile
regeneration below are done; neither is possible from this Mac.

### Findings

- Confirmed: `check-deps.sh` passed when `cargo tree` failed. A missing crate
  printed "dependency graph ok" and exited 0 (red run of the new
  `scripts/check-deps.test.sh`). The graph is now captured first, a failure
  exits 1 with "cargo tree failed for <crate>", and the script only runs its
  checks when executed, so the test can source it.
- Confirmed: `check-public-repo.py` missed `socialspeak-pins.json` and any
  `.wav`/`.zip` below the root. It now matches `*pins.json` everywhere and
  `.wav`/`.zip` in every directory, except `.wav` under a `tests/fixtures/`
  directory (the licensed VAD fixture is the only tracked one).
  `scripts/check-public-repo.test.py` covers both.
- Confirmed on OpenSSL 3.0.22 (Debian bookworm, the same 3.0 series as
  Ubuntu 22.04/24.04): `openssl x509 -checkhost` prints "does NOT match" and
  exits 0, so the renewal hook's hostname check never failed. OpenSSL 3.5
  exits 1. The hook now compares the verdict line. Exercised in a bookworm
  container with stubbed `nginx`/`systemctl`: a matching certificate
  installs (`root:65532 0640`) and reloads; a wrong hostname exits 1 before
  anything is installed; a failing `nginx -t` exits 1 after the bundle is
  installed, without reloading.
- Confirmed: `ss-video --no-default-features` did not build under
  `-D warnings` (the H.264 NAL helpers are used only by the capture
  publisher) and its OpenH264 round-trip test failed without `software`.
  Fixed by gating `mod h264` on `capture` and the test on `software`.
- Confirmed: `cargo deny check` skipped optional dependencies. With
  `--all-features` the graph gains `moq-vaapi`, `moq-nvenc`, `cudarc`,
  `bindgen 0.70.1` and `libloading 0.9.0` (868 to 873 entries). `deny.toml`
  now sets `[graph] all-features = true`; a probe ban on `cudarc` fails only
  with that setting. Licenses, bans and sources pass either way.
- New, not fixed here: `cargo deny check advisories` fails today on
  RUSTSEC-2026-0459 (bundled SQLite before 3.54.0, in `libsqlite3-sys`
  0.37.0). `sqlx-sqlite` 0.9.0 requires `libsqlite3-sys <0.38`, so the fix is
  a sqlx release that allows 0.39; until then CI's `cargo deny` step fails.
  The exposure is opening a database together with a `-wal` that a less
  trusted party wrote; the restore commands delete the WAL files.
- Confirmed by reading and by a Docker analogue: the backup tar records
  `socialspeak.db` as uid 0, gid 0 (`tar::Header::new_gnu`). Extracting it as
  root left the database `0:0`, and the server then failed with "attempt to
  write a readonly database". Extracting as uid 65532 (the documented Docker
  restore) kept `65532:65532`, removed the WAL and restarted healthy.
- Read from systemd semantics, not run: a transient unit with
  `DynamicUser=yes` and no `User=` gets a dynamic user named after
  `run-u….service`, and systemd chowns an existing `StateDirectory=` to the
  unit's user on start when the top directory's owner differs. The old
  backup command therefore chowned `/var/lib/private/socialspeak` away from
  the running server until its next start. The commands now pass
  `User=socialspeak`, sharing the service's dynamic UID, and the restore
  streams the tar into `tar` running as that user.
- Confirmed: `conn list` refused the container's `0.0.0.0:9101` internal
  bind. It now connects to loopback for a wildcard bind and still refuses
  any other address (`crates/ss-server/src/main.rs`, with unit tests). That
  also gave the image a healthcheck: there is no shell or curl in it.
- Confirmed: `cap_add: NET_BIND_SERVICE` was redundant. With
  `cap_drop: [ALL]`, `no-new-privileges` and `read_only` (plus a `/tmp`
  tmpfs), both Compose files still bind 443 as uid 65532 and became healthy
  in the local smoke run.
- NGINX: `nginx -t` passes on nginx 1.30.6 (alpine image). With the
  upstream down, 60 quick requests from one address gave 53 x 502 and
  7 x 429, so `limit_req` applies before the proxy. All directives exist in
  Ubuntu's 1.18 and 1.24; not run there.
- Document drift fixed: T-034 notes (superseded bullet), the aarch64 musl
  linker in `.cargo/config.toml` (native `musl-gcc`, matching the release
  workflow and Dockerfile), the README's Linux package pointer, the
  `deepfilter` stub and `pnpm check` definition in `04-conventions.md`, and
  the timeout and runner claims in `docs/dependency-maintenance.md`.
  `public-repo.yml` was the only job left without `timeout-minutes`; it has
  15 now. `windows-latest` is now `windows-2025` in `ci.yml`,
  `test-desktop.yml` and `scripts/release-desktop.mjs`. `fuzz.yml` still
  says `ubuntu-latest`: T-101 owns that file.

### Changes

- Checks: `scripts/check-deps.sh` (fail closed), `scripts/check-deps.test.sh`,
  `scripts/check-public-repo.py`, `scripts/check-public-repo.test.py`,
  `scripts/check-desktop-toolchain.mjs` and its test. `just check-deps` runs
  the test first. `just check-desktop` first runs the toolchain check (Node
  must satisfy `engines.node`, pnpm must equal `packageManager`; Node is
  checked first so a wrong Node never starts pnpm). Here, with Node 24.22,
  it stops with "Node 24.22.0 is outside engines.node >=26.13.0 <27; CI uses
  26.13.0." `apps/desktop/.prettierignore` lists `pnpm-lock.yaml`; the
  lockfile content is unchanged.
- Workflows: `ci.yml` adds `ss-audio --features aec` tests and lints,
  `ss-video --no-default-features` tests and lints on every OS,
  `ss-video --features vaapi,nvidia` lints on Linux, and the check-deps test.
  `check-frontend.yml` runs the toolchain check and its test.
  `public-repo.yml` runs the hygiene test and has a timeout. No trigger
  changed: every workflow is still `workflow_dispatch` (or `workflow_call`).
- Deploy: hardened Compose files with a healthcheck; NGINX per-client
  `limit_req`/`limit_conn` and an upstream keepalive pool (`max_conns=48`,
  under the server's 64 per address); renewal hook installs first, then
  tests and reloads; `smoke.sh` starts from new volumes, removes containers,
  volumes and its log, and runs both Compose files with only host ports
  (and the NGINX certificate directory) replaced; backup and restore
  commands rewritten in `deploy/README.md`; `conn list` in a container
  documented in `deploy/grafana/README.md`.
- Files outside the Paths: `justfile`, `crates/ss-server/src/main.rs`,
  `crates/ss-video/src/lib.rs`, `crates/ss-video/src/decode.rs`,
  `scripts/release-desktop.mjs` (runner label), the new
  `scripts/check-*.test.*` and `scripts/check-desktop-toolchain.mjs`,
  `docs/plan/04-conventions.md`, `docs/plan/tasks/T-034-deployment.md`,
  `deploy/grafana/README.md` (under `deploy/`), and this task's `INDEX.md`
  row.
- Merge with T-092: both edit the two Compose files and `smoke.sh`. Here the
  hardening keys sit right after `command:`, the removed `cap_add` lines
  were the last lines of the service, and the `smoke.sh` changes are the
  setup/cleanup block and an appended Compose stage. The spike `docker run`
  line T-092 changes is untouched. If T-092 appends `ulimits` after
  `cap_add`, expect a small conflict at the end of each service.

### Validation here (macOS, Docker Desktop 29.9, linux/aarch64)

- `bash scripts/check-deps.test.sh` red before the fix, green after;
  `bash scripts/check-deps.sh` passes.
- `python3 scripts/check-public-repo.test.py` red before, green after; the
  real check passes on the tracked tree.
- `node --test scripts/*.test.mjs` passes, including the new toolchain test.
- `cargo clippy --workspace --all-targets -- -D warnings`, nightly
  `cargo fmt --check`, `cargo test -p ss-server --bin ss-server`, and every
  new CI feature step except `vaapi,nvidia` on Linux, run on macOS.
  `ss-video --features vaapi,nvidia` lints clean on macOS, where both
  features only add Linux-only dependencies; the Linux build of
  `moq-vaapi` (bindgen, libclang) and `moq-nvenc`/`cudarc` is unverified
  until CI runs on Linux. No hardware encoder was exercised.
- `docker compose config` passes for all three Compose files.
- `deploy/smoke.sh` passed end to end with overridden host ports (another
  `ss-server` held 19101 here): healthz, HTTPS, plaintext refused, two bots
  with `speakers=2`, then both Compose files healthy with HTTPS and
  `conn list`. No container or volume was left.
- `actionlint` is not installed here; workflow files were parsed as YAML only.
- The Docker restore was run with `alpine:3.20`; the README names
  `alpine:3.22` (same BusyBox `tar` behavior), which was not pulled.

### Tommy's checklist

- [ ] Regenerate the lockfile with the pinned toolchain and commit it as
  pnpm writes it: with Node 26.13.0 and pnpm 12.18.0,
  `cd apps/desktop && pnpm install && cd ../.. && just check-desktop`, then
  `git status` must show only `pnpm-lock.yaml` (or nothing). Running
  `pnpm check` again must leave `git status` clean.
- [ ] CI decision: answered 2026-11-11, keep manual-only (recorded above).
  Dispatch `ci.yml` on this branch before merging it: it is the first run
  of the new feature steps, the check-deps test and the Compose smoke on
  Linux, and the first `windows-2025` run. Expect `cargo deny` to fail on
  RUSTSEC-2026-0459 until sqlx allows `libsqlite3-sys` 0.39.
- [ ] Dispatch `public-repo.yml` once to run the new hygiene test with
  Gitleaks.
- [ ] VPS backup and restore (Ubuntu, systemd install), recording ownership
  with `stat` at each step. Use numeric IDs, since the dynamic user name
  resolves only while allocated:

  ```sh
  own() { sudo stat -c '%u:%g %a %n' /var/lib/private/socialspeak \
    /var/lib/private/socialspeak/socialspeak.db \
    /var/lib/private/socialspeak/socialspeak.db-wal \
    /var/lib/private/socialspeak/attachments 2>&1; }
  pid=$(systemctl show -p MainPID --value socialspeak); ps -o uid=,gid= -p "$pid"
  own                                   # 1. running server: all owned by its UID
  sudo systemd-run --wait --pipe --collect --property=User=socialspeak \
    --property=DynamicUser=yes --property=StateDirectory=socialspeak /usr/bin/id
                                        # 2. must print the server's UID
  # 3. the backup command from deploy/README.md, then:
  own; sudo stat -c '%u:%g %a %n' /var/lib/private/socialspeak/socialspeak.tar
                                        #    unchanged; the tar owned by the same UID, 0600
  # 4. copy it off with `sudo cat`, stop the server, run the restore command, then:
  own                                   #    db owned by the same UID, 0600; no -wal
  sudo systemctl start socialspeak && own   # 5. unchanged, server writes again
  journalctl -u socialspeak -n 20       #    no "readonly database"
  ```

  Optional, only on a throwaway VPS: run the old command (without
  `--property=User=socialspeak`) while the server runs; step 1's `own`
  should then show the directory under another UID until
  `sudo systemctl restart socialspeak`. Record the outputs here.
- [ ] On that VPS, run the renewal hook once with a real certificate and
  confirm NGINX accepts the new `limit_req_zone`/`upstream` lines
  (`sudo nginx -t`).
