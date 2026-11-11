# T-099 — Reconcile the plan with the code; make acceptance status visible

Status: awaiting-acceptance
Phase: 4
Size: S
Depends on: —
Paths: `docs/plan/`, `docs/design/decisions.md`, `docs/security.md`, `scripts/`, `justfile`, `crates/*/README.md`, `.github/workflows/ci.yml`

## Goal
The plan says what the code does, and one page shows what is waiting for
hands-on testing as opposed to unfinished.

Source: 2026-11-09 review at `d861a0a`. Documentation only; code changes
belong to the tasks named below.

## Scope
- `02-protocol.md`:
  - §1 and §6 still describe pin-only trust; the architecture and security
    documents were updated for public-certificate trust, this one was not.
  - §6 lists `GET /api/v1/channels/{id}/search?q=`; the implementation is
    `GET /api/v1/search` with `channel`, `from`, `before`, `after`, `has`.
  - §6 lists `/api/v1/users/{id}/avatar`; no such route exists. Remove it
    or open a task.
  - History `?after=`, and `Message.webhook_id`/`webhook_name`, are
    undocumented.
  - `MemberJoined`/`MemberLeft` are listed and never sent (T-094).
- `00-overview.md` D10 and `docs/design/decisions.md` D10 describe TOFU
  plus optional ACME only; add the public-certificate policy.
- D3 says no MoQ catalog or transport type is linked and names `moq-video`
  0.1.18. `ss-video` pins `moq-video` 0.1.21 and uses
  `hang::catalog::VideoConfig` and `moq_net::Timestamp`/`Rate`. Either drop
  those types or state exactly which are allowed.
- `01-architecture.md`: the voice registry is described as a read-heavy
  `RwLock`; the code uses one mutex (T-094 decides which wins). The
  directory tree calls the repository `socialspeak/`; the product is
  Sideband.
- `03-data-model.md`: the owner rule and "only owner-created roles carry
  ADMINISTRATOR" are not implemented (T-090 decides which wins). The schema
  listing does not mention what migrations 0002, 0003 and 0005 added
  (`mentions_everyone`, webhook name, message nonces) and still calls
  full-text search a phase 4 task although migration 0006 ships it.
- `04-conventions.md`: crate READMEs are limited to 40 lines; `ss-audio`,
  `ss-client-core` and `ss-server` exceed it. Trim them or raise the limit.
- `tasks/INDEX.md`: the Mermaid graph lacked 46 of the table's dependency
  edges on 2026-11-09 and the waves table omits T-045A–C. Generate the
  graph and the waves from the table with a script under `scripts/`, and
  check them in CI, or delete both.
- Status values: 18 tasks are `in-progress`, most with code complete and
  manual acceptance open. Add an `awaiting-acceptance` status, apply it,
  and add one acceptance matrix (platform × check, with the owning task)
  so the remaining hands-on work is on one page. T-065 is `done` while its
  dependency T-045 is `in-progress`; resolve it.
- `docs/security.md` still says the second-person review is pending
  (T-067). Add the findings of this review that change the stated
  guarantees, or link the tasks.

## Out of scope
Changing behaviour. Rewriting the plan's structure.

## Acceptance criteria
- A script regenerates the INDEX graph and waves and fails when they are
  stale.
- Every route and message in `02-protocol.md` exists in the code, checked
  by a listed command or test.
- The acceptance matrix exists and every `in-progress` task is either
  genuinely in progress or marked `awaiting-acceptance`.

## Notes from implementation

Done at `0d0cb16` (2026-11-11), after waves 1 to 4 merged. Each scope item
was checked against the code, not the task text. Documentation and scripts
only; no Rust changed.

**Scope items**

| Item | Outcome |
|---|---|
| `02-protocol.md` §1/§6 pin-only trust | Fixed: §1 describes `TrustPolicy` (public certificate vs pin, no TOFU fallback); §6 HTTPS uses the QUIC session's policy ("pinned HTTPS") |
| §6 search route | Fixed: `GET /api/v1/search` with `q`, `channel`, `from`, `before`, `after`, `has`, `limit`, results and newest-first order documented |
| `/api/v1/users/{id}/avatar` | Removed: avatars and the server icon are served by the attachment download route (`may_read` allows them to any member). No task needed |
| History `?after=`, `Message.webhook_id`/`webhook_name` | Fixed in §3 and §6 |
| `MemberJoined`/`MemberLeft` | Already fixed by T-094 (`MemberLeft` reserved); the ban question is in T-106 |
| D10 public certificates | Fixed in `00-overview.md` and `decisions.md` D10 |
| D3 `moq-video` version and types | Fixed: pinned `=0.1.21`; the allowed MoQ value types are listed (`moq_net::Timestamp`, `moq_net::bandwidth::Rate`, `hang::catalog::VideoConfig`/`VideoCodec`, inside `ss-video` plus one desktop example) |
| `01-architecture.md` voice lock | Already fixed by T-094 (`parking_lot::RwLock`) |
| `01-architecture.md` repository name | Fixed (`sideband/`); avatars line corrected; `fuzz/` and `scripts/` added to the tree |
| `03-data-model.md` owner rule | Already fixed by T-090 (administrators are peers; T-106 asks the maintainer to confirm) |
| `03-data-model.md` migrations | Fixed: `mentions_everyone` (0002), `webhook_name` (0003), hashed invites (0004), `message_nonces` (0005), FTS5 `message_search` (0006, shipped); `user grant-admin` corrected to `role grant-admin` here and in `security.md` |
| Crate READMEs over 40 lines | Trimmed `ss-audio` (46 → 39), `ss-client-core` (43 → 40), `ss-server` (45 → 40); the limit stays. Stale content fixed on the way: client reconnect backoff and nonce replay, the server CLI list, the `unsafe` modules |
| INDEX graph and waves | Generated: `scripts/plan-index.py` (below) |
| Status values | Added `awaiting-acceptance` (INDEX, `00-overview.md`, `04-conventions.md`, `05-testing-app.md`) and the matrix [ACCEPTANCE.md](ACCEPTANCE.md) |
| T-065 `done` on in-progress T-045 | Resolved: T-065 depends on what it uses, T-045A and T-045B (code merged); the generator now fails when a `done` task depends on unmerged work |
| `docs/security.md` | Added section 13 (what the review changed, per task), five known gaps with their tasks, the T-092 link-token layout and per-signer pool, and a pointer from the review-status note |

**Statuses.** Every `in-progress` task except T-096, T-102 and T-103 (wave
5's, left alone) was classified from its notes and spot checks of the code:

- `awaiting-acceptance` (code merged, only hands-on checks or a maintainer
  decision left): T-043, T-044, T-045, T-045B, T-045C, T-049, T-050, T-051,
  T-055, T-057, T-058, T-063, T-071, T-072, T-073, T-075, T-076, T-081 to
  T-086, T-088, T-089, T-091, T-093, T-095, T-097, T-098, and T-099.
- `done` (every acceptance criterion automated and met, nothing left in
  scope): T-090, T-092, T-101, and T-094 (its VPS latency rerun belongs to
  T-069 and its ban question to T-106).
- Still `in-progress`: T-048, whose low-level hook fallback is neither built
  nor waived (the waiver question is in T-106).

Stale notes corrected in T-043 (checklist already opens real settings),
T-051 (diagnostics are available) and T-049 (focus recheck not wired; T-108).

**New tasks** (each claim checked in the code):

- T-106: maintainer decisions (ban and `MemberLeft`, owner, Linux rtkit,
  capture channel count, PTT arming, Windows hook fallback).
- T-107: client transport and CLI follow-ups (256-event router overflow
  closes the connection, `ss-cli` own-track lookup by user, `ss-cli` closing
  quantum, resync `Retry-After` and mention counts).
- T-108: desktop follow-ups (focus recheck, `ListDms` paging, `ServerError`
  retry hint, unknown permission bits, profile-change rejoin).
- T-109: server follow-ups (`Rooms::join` drops leave events when the
  channel is full, `channel_members(user_id)` index, Welcome size bound,
  flaky webhook-order test).
- T-110: CI and release follow-ups (`fuzz.yml` on `ubuntu-latest`, stale
  `fuzz/Cargo.lock`, RUSTSEC-2026-0459, Windows signing inside `tauri
  build`, Dependabot pins).

**Scripts** (Python 3 standard library only; Python is already used by
`check-public-repo.py`):

- `scripts/plan-index.py` reads every INDEX table with `ID`, `Depends on`
  and `Status` columns (found by header name, so row edits and new tables
  are fine), and rewrites the graph and waves between the
  `plan-index:graph` and `plan-index:waves` markers. Output depends only on
  ids and dependencies, so status edits never make it stale. It also fails
  on a cycle, an unknown dependency, an unknown status, a duplicate row, a
  row that disagrees with its task file's `Status:` or `Depends on:`, and a
  `done` task with an unmerged dependency. Idempotent: `just plan-index`
  after merging sibling branches, `just plan-index-check` to verify.
- `scripts/check-protocol-doc.py` compares the `| Method | Path |` table in
  `02-protocol.md` with every axum `.route(..)` under `crates/ss-server/src`
  (both directions, methods included, parameter names and queries ignored),
  and the Client → Server and Server → Client tables with the
  `ClientEnvelope`/`ServerEnvelope` `oneof msg` variants and the `.proto`
  messages. It found the search and avatar rows above and the undocumented
  `/healthz`, `/connections` and ACME challenge routes, now listed.
- Tests: `python3 scripts/plan-index.test.py`,
  `python3 scripts/check-protocol-doc.test.py`.
- `just plan-check` runs both tests and both checks (well under a second)
  and is part of `just check`; `ci.yml` (still dispatch-only) runs the same
  four commands on Linux.

**Verification.** `just check` and both script tests; see the commit.

