# Contributing

SocialSpeak is under active development. Start with the [task index](docs/plan/tasks/INDEX.md)
and discuss substantial work in an issue before opening a large pull request.
An implemented feature can still have outstanding manual acceptance; preserve
that distinction in documentation and reports.

Read the [engineering conventions](docs/plan/04-conventions.md). Changes to voice,
transport or input also need the [design decisions](docs/design/README.md).
Keep protocol fields append-only and database migrations immutable.

## Validation

Install the prerequisites in the [README](README.md), then run:

```sh
just check
just check-desktop
python3 scripts/check-public-repo.py
node --test scripts/build-test-desktop.test.mjs
```

Run targeted tests while developing, then the required checks before submitting.
Describe OS, relevant hardware, commands run, and any manual tests not performed.
Use the app's guided evidence export for hearing/device/game observations; do not
mark those checks passed on the strength of a mock or synthetic audio test.

Use conventional commit messages and a focused branch. In the pull request,
explain the problem, resulting behavior, validation and relevant limitations.
Update task/design documentation when behavior or acceptance changes.

## Keep private data out

Do not commit server data, TLS keys, accounts/sessions, invitations, private logs,
real voice recordings or exported evidence. The ignore rules are a convenience,
not a security boundary: review `git diff --cached` before committing. Synthetic
or explicitly licensed test assets need documented provenance and consent where
applicable. Never put vulnerability details into a public issue; see [SECURITY.md](SECURITY.md).

Source contributions use the project's existing MIT OR Apache-2.0 license unless
explicitly stated otherwise. Preserve third-party notices and the separate CC BY
license on the speech fixture. Distribution license review remains a release task.
