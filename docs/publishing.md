# Publishing the source repository

This repository is a development preview. Publishing the source does not close
the installed-app, voice-quality, security or production-distribution gates in
the implementation plan.

Before the first push:

1. Run `python3 scripts/check-public-repo.py`, `just check` and
   `just check-desktop`. Scan the intended branch history with Gitleaks.
2. Check `git config user.email` uses your intended public identity. History
   rewrites change commit IDs; backups and reflogs can retain old identities
   locally and should remain private.
3. Create an empty GitHub repository and add its URL as `origin`. Push only the
   intended publication branch, for example
   `git push -u origin HEAD:main`. Do not use `--mirror` or `--all`: local task
   branches and tool refs are not part of the initial publication.
4. Set `main` as the default branch. Enable private vulnerability reporting,
   secret scanning and push protection where available. Configure branch rules
   to require review and passing checks once the initial workflows have run.
5. Verify the hosted Windows, macOS and Linux jobs. Locally passing checks do
   not establish that every hosted runner or installed package works.

The server release workflow publishes on `server-v*` tags. Push release tags
only when intentionally publishing binaries. Desktop test packages remain
unsigned/ad-hoc signed; signing, updating and complete binary dependency notices
still require release work. See [third-party notices](../THIRD_PARTY_NOTICES.md).

Local worktrees, recordings, runtime databases, credentials and evidence exports
are ignored. Do not force-add them. The repository path checker is a guardrail,
not a substitute for reviewing diffs and running the secret scanner.
