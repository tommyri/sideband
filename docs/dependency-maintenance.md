# Dependency maintenance

Last reviewed: 2026-11-11 (workflow and toolchain sections, T-098 and T-110).

GitHub workflows remain manually triggered. Dependency maintenance must not add
push, pull-request, or scheduled CI runs. Dependabot may propose updates; merge
only after reviewing compatibility and running the relevant checks.

## Toolchains and Actions

- Rust stable is pinned in `rust-toolchain.toml` and aligned with workflow inputs.
- Desktop development uses Node 26.13.0 and pnpm 12.18.0. The desktop package
  records its Node engine and package-manager version; CI uses matching versions.
  `scripts/check-desktop-toolchain.mjs` (run by `just check-desktop` and the
  frontend workflow) fails when either differs. Prettier ignores
  `pnpm-lock.yaml`: the lockfile is committed exactly as pnpm 12.18.0 writes it.
- GitHub Actions use immutable commit SHAs, annotated with their release versions.
  Resolve updated SHAs from each official action repository, and review migration
  notes before changing major versions.
- Apple Silicon builds use `macos-15`; optional Intel builds use `macos-15-intel`.
  The retired macOS 14 runners cannot build new releases.
- Runner images are pinned: `ubuntu-24.04` (and `ubuntu-24.04-arm` for the aarch64
  server), `macos-15`, `windows-2025`. Move a label deliberately, not through a
  `*-latest` alias. Dependabot does not update runner labels.
- Dependabot covers the actions (by commit, including `dtolnay/rust-toolchain`,
  whose `v1` tag every workflow pins), the Docker base images in `deploy/`, the
  BuildKit image of the server release (`.github/buildkit/Dockerfile`, read by
  `release-server.yml`), and both Cargo lockfiles. It does not cover the Rust
  version, the fuzzing nightly, or the tools `taiki-e/install-action` installs
  (`cargo-deny`, `cargo-fuzz`: newest at run time). `fuzz/Cargo.lock` is
  refreshed from the workspace lockfile; see `fuzz/README.md`.

## Frontend compatibility

TypeScript 7 compiles the app. `typescript-eslint` still requires the TypeScript 6
JavaScript API, so the package follows Microsoft's supported alias arrangement:
`@typescript/native` resolves to TypeScript 7 and `typescript` resolves to
`@typescript/typescript6`. Remove the compatibility alias only when lint tooling
supports the new API. See [TypeScript's migration guidance](https://devblogs.microsoft.com/typescript/announcing-typescript-7-0/#running-side-by-side-with-typescript-6-0).

Vite 8 uses Oxc minification. The Safari 16 target matches the existing macOS 13
minimum and supports the BigInt permission masks; Windows targets Chrome 105.

Keep pnpm's release-age protection. A few just-published package versions can
remain ineligible even after `pnpm update --latest`. The scoped minimatch override
and release-age exception address [GHSA-g3hv-75wg-m249](https://github.com/advisories/GHSA-g3hv-75wg-m249)
in lint tooling; remove them when upstream dependencies require a patched version.

## Rust compatibility and checks

Direct dependencies and Cargo.lock were reviewed against crates.io stable
releases, including major updates. Keep pre-release TLS versions out until the
QUIC stack supports them. `quinn-udp` stays on 0.5 to match Quinn 0.11: its
macOS batching feature must apply to the actual transport dependency, rather
than an unused parallel 0.6 copy. The reqwest and ACME features explicitly retain the
ring TLS provider. Existing Argon2 PHC hashes remain valid; a regression test uses
a reference hash from the previous Argon2 version.

`cargo deny check` enforces the repository's license and advisory policy. It
checks the graph with every feature enabled (`[graph] all-features` in
`deny.toml`); before 2026-11-11 the optional VA-API and NVENC crates were never
checked. Scoped
maintenance-only exceptions remain for unmaintained transitive helpers that have
no safe upstream upgrade. These are recorded in `deny.toml`; they do not disable
vulnerability checks generally. One vulnerability is ignored, by id and with a
review date: RUSTSEC-2026-0459 (bundled SQLite), until an sqlx release allows
`libsqlite3-sys` 0.39. Its exposure is in `docs/security.md` §11.

Before publishing a dependency refresh, run frontend checks and production build,
workspace tests and strict Clippy, packaging-script tests, npm audit, and cargo
deny. Build the selected release platforms to catch native API differences.

### Real-time audio on Linux

Added 2026-11-16 (T-117). `ss-audio` enables
`audio_thread_priority/with_dbus` on Linux only, so the voice threads ask
rtkit for real-time priority over the system D-Bus. That adds `dbus` 0.6.5
(MIT OR Apache-2.0) and links the system `libdbus-1` through `libdbus-sys`
0.2.7 (MIT OR Apache-2.0; already used by tao). The build needs
`libdbus-1-dev`, which `install-linux-deps.sh` installs for the `desktop`
and `audio` sets. `audio_thread_priority` 0.35 requires `dbus` 0.6, so it
sits beside tao's 0.9 until upstream moves; `cargo deny` reports this as a
duplicate warning. `signal-hook` 0.3 (MIT OR Apache-2.0, already in the
lockfile through crossterm) catches the `SIGXCPU` that rtkit's
`RLIMIT_RTTIME` soft limit would otherwise make fatal. macOS and Windows
builds do not change.

## CI runner time

All entry points remain manual. CI and desktop packaging call the same frontend
workflow once on Linux, and wait for it before starting native builds. Every
selected OS still compiles and tests the native desktop code; installer builds
still compile their own platform-specific frontend assets.

The general CI job runs workspace tests/lints once (which already include the
desktop crate), rather than repeating them through `just check-desktop`. Formatting
runs only on Linux. Audio feature combinations, input integration, container smoke,
and reconnect chaos coverage remain. Select the `benchmarks` input when performance
measurements are needed; these otherwise add separate release-profile builds.

Compatible native CI and packaging jobs share target-specific Rust dependency
cache keys. Cargo/toolchain/environment hashing remains enabled. Installer archives
upload without redundant compression and temporary build artifacts expire after
seven days; published release assets are unaffected.

Linux setup uses `scripts/install-linux-deps.sh`: two network retries, 30-second
network timeouts, bounded package-lock waits, and separate update/install deadlines.
Every job sets `timeout-minutes` (a job that calls a reusable workflow gets the
called jobs' limits), so a stalled run does not consume the default six hours. A slow mirror now
fails explicitly instead of occupying a runner indefinitely; rerun only failed jobs
for transient infrastructure errors. Cold builds still take longer after dependency
or toolchain upgrades, and cache hits do not eliminate native linking or packaging.

Validate workflow edits with `actionlint` and `bash -n scripts/install-linux-deps.sh`.
Measure actual run durations before claiming a percentage improvement.
