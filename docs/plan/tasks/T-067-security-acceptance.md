# T-067 — Security acceptance: long fuzz runs, document review, licence policy

Status: todo
Phase: 4
Size: S
Depends on: T-061
Paths: `docs/security.md`, `docs/plan/tasks/T-061-security-review-fuzzing.md`, `deny.toml`

## Goal
Finish the T-061 acceptance criteria that needed more time or a second
person than one agent session had.

## Scope
- Run each of the five fuzz targets for one hour with the T-061 flags:
  `cargo +nightly fuzz run <target> -- -max_total_time=3600 -timeout=10 -rss_limit_mb=2048 -max_len=8192`
  for `control_frames`, `media_headers`, `mentions`, `preview_html`, and
  `multipart_upload`. Record duration, executions, and result per target in
  the T-061 notes. Any crash or timeout becomes a fix with a regression
  test in the same change.
- A second person reviews `docs/security.md` and adds their name and the
  date at the top. Address review comments in the same change.
- Decide the `deny.toml` MPL-2.0 exception for `audio_thread_priority`
  (pulled by `cpal`, client only): keep it with a one-line rationale, or
  replace the dependency.
- Confirm the nightly `fuzz.yml` workflow runs green once a GitHub remote
  exists; until then note that it has only been checked locally.

## Out of scope
New hardening work (T-068); TLS for REST (T-066).

## Acceptance criteria
- Table in the T-061 notes shows one hour clean per target.
- `docs/security.md` names its reviewer.
- `deny.toml` carries the licence decision with a comment.

## Notes from implementation
