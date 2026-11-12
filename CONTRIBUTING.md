# Contributing

Sideband is under active development. Start with the [task index](docs/plan/tasks/INDEX.md)
and discuss substantial work in an issue before opening a large pull request.
An implemented feature can still have outstanding manual acceptance; preserve
that distinction in documentation and reports.

Read the [engineering conventions](docs/plan/04-conventions.md). Changes to voice,
transport or input also need the [design decisions](docs/design/README.md).
Keep protocol fields append-only and database migrations immutable.

## How tasks work

The plan lives in [`docs/plan/`](docs/plan/00-overview.md). Work is split into
task files, `docs/plan/tasks/T-xxx-name.md`, each with a goal, scope, the
paths it may touch, acceptance criteria and "Notes from implementation".
[INDEX.md](docs/plan/tasks/INDEX.md) lists every task with its status and
dependencies, and generates the dependency graph and waves from those
tables (`just plan-index`).

- Pick a `todo` task; its INDEX row names the tasks it depends on and the
  waves show what can run in parallel. Work on a branch named
  `task/T-xxx-short-name`, one task per pull request.
- Stay within the task's paths. If you need to change something another
  task owns, say so in the pull request.
- When the code is done, set the same status in the task file and its INDEX
  row, and add "Notes from implementation": what changed, deviations from
  the task file, and what you could not test.
- Use `awaiting-acceptance` when only hands-on checks remain (real hardware,
  a VPS, GitHub, or a maintainer decision). Each such check is a row in the
  [acceptance matrix](docs/plan/tasks/ACCEPTANCE.md). `just plan-check` fails
  when a row is missing or a status disagrees.

The status values and their exact meaning are in
[plan documents](docs/plan/04-conventions.md#plan-documents).

## Development setup

All platforms need:

- the Rust version pinned in [rust-toolchain.toml](rust-toolchain.toml);
- nightly Rust, only for formatting:
  `rustup toolchain install nightly --profile minimal --component rustfmt`;
- [just](https://github.com/casey/just), CMake, a C/C++ toolchain and
  Python 3 (the repository checks use only its standard library);
- for the desktop app, Node and pnpm at the versions in
  [`apps/desktop/package.json`](apps/desktop/package.json) (`engines` and
  `packageManager`). Another pnpm rewrites the lockfile; `just check-desktop`
  stops first, with the reason, when the versions differ.

Per operating system:

- **macOS:** Xcode Command Line Tools and CMake; deployment target macOS 13+.
- **Windows:** Visual Studio C++ build tools (MSVC), CMake and WebView2.
  Not part of the maintainer's routine checks; CI covers it when dispatched.
- **Linux:** ALSA/PipeWire development headers, Clang, CMake, WebKitGTK 4.1,
  appindicator and the other packages that
  [`scripts/install-linux-deps.sh desktop`](scripts/install-linux-deps.sh)
  installs (the script targets Ubuntu runners). Current Linux test packages
  target Ubuntu 24.04; older-distribution portability has not been
  established.

Then:

```sh
cd apps/desktop
pnpm install --frozen-lockfile
cd ../..
just check
just check-desktop
just dev-desktop
```

`just --list` shows every recipe. `just run-server` starts the
unauthenticated voice spike that `ss-cli` speaks; for the real server see the
[README quick start](README.md#quick-start).

## Validation

Run targeted tests while developing, then the required checks before
submitting:

```sh
just check
just check-desktop
python3 scripts/check-public-repo.py
node --test scripts/build-test-desktop.test.mjs
```

`just check` is formatting, clippy, the workspace tests and `just plan-check`.
`plan-check` also runs the documentation checks: `just doc-links` (every
relative link and `#anchor` in the repository's Markdown resolves; external
URLs are not checked) and `just proto-doc-check` (the
[message reference](docs/protocol/messages.md) matches the `.proto`
comments; regenerate it with `just proto-doc`).

Describe OS, relevant hardware, commands run, and any manual tests not performed.
Use the app's guided evidence export for hearing/device/game observations; do not
mark those checks passed on the strength of a mock or synthetic audio test.

### CI runs only when dispatched

Every GitHub Actions workflow is started by hand; pushes and pull requests run
nothing (maintainer decision, 2026-11-11). The routine gate is local: `just check`,
plus `just check-desktop` for desktop or frontend changes. A Mac does not see
Windows or Linux breakage or leaked secrets, so dispatch:

- **CI** (`ci.yml`) on the branch before merging changes to platform code, feature
  flags, dependencies, `deploy/` or workflows: Ubuntu, macOS and Windows checks,
  the container and Compose smoke test, `check-deps`, `cargo deny` and the
  documentation checks. The release workflows also run it for the commit they
  publish.
- **Public repository checks** (`public-repo.yml`) before pushing to the public
  repository and before every release: tracked-file hygiene and the Gitleaks scan.
- **Desktop test builds** after packaging changes, and **Fuzz** after parser changes.

## Pull requests

Use conventional commit messages and a focused branch. In the pull request,
explain the problem, resulting behavior, validation and relevant limitations.
Update task/design documentation when behavior or acceptance changes. Keep
user-facing documentation current with the behavior: the
[user guide](docs/user-guide.md) and [self-hosting guide](docs/self-hosting.md)
link to the documents that hold each fact rather than copying them, so update
the source and keep the links working.

## Keep private data out

Do not commit server data, TLS keys, accounts/sessions, invitations, private logs,
real voice recordings or exported evidence. The ignore rules are a convenience,
not a security boundary: review `git diff --cached` before committing. Synthetic
or explicitly licensed test assets need documented provenance and consent where
applicable. Never put vulnerability details into a public issue; see [SECURITY.md](SECURITY.md).

Source contributions use the project's existing MIT OR Apache-2.0 license unless
explicitly stated otherwise. Preserve third-party notices and the separate CC BY
license on the speech fixture. Distribution license review remains a release task.
