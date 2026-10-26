# T-043 — Onboarding UI: add server, trust fingerprint, register/login, multi-server

Status: todo
Phase: 3
Size: M
Depends on: T-042
Paths: `apps/desktop/src/features/onboarding/`

## Goal
A friend receives `host:port` + an invite code and is talking within two
minutes without reading docs.

## Scope
- "Add server" dialog: address input (accepts `host`, `host:port`,
  `ss://host:port`), live reachability check via `GET /api/v1/server`
  (name, registration mode, fingerprint).
- Certificate trust screen: show fingerprint (grouped hex), server name,
  "Trust this server" → pin; changed-fingerprint warning screen with explicit
  re-trust (red, explains the risk).
- Register (invite code, username, password with strength hint) and Login
  forms; errors mapped from `Error.code` to friendly text; remember session.
- Server rail: list of added servers with unread badges, reorder, remove
  (with confirm), per-server connection status indicator; reconnecting
  banner.
- First-run checklist after joining: pick mic/output, choose PTT key or
  voice activation (links to T-051 settings).

## Out of scope
Settings internals.

## Acceptance criteria
- Manual script in the task notes executed on all three OSes.
- Vitest component tests for form validation and error mapping.
- Keyboard-only flow works end to end.

## Notes from implementation
