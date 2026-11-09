# T-099 — Reconcile the plan with the code; make acceptance status visible

Status: todo
Phase: 4
Size: S
Depends on: —
Paths: `docs/plan/`, `docs/design/decisions.md`, `docs/security.md`, `scripts/`

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
