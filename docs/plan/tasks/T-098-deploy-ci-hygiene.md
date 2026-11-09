# T-098 — Deploy and CI hygiene: fail-closed checks, compose and NGINX limits, backup ownership

Status: todo
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
