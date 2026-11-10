# T-105 — Remaining permission projections use ss-client-core authority

Status: todo
Phase: 3
Size: S
Depends on: T-095, T-096
Paths: `crates/ss-client-core/src/authority.rs`, `apps/desktop/src-tauri/src/role_admin.rs`, `apps/desktop/src-tauri/src/stream.rs`, `apps/desktop/src-tauri/src/chat.rs`, `apps/desktop/src-tauri/src/channel_admin.rs`

## Goal
Every "may the user do this" hint the shell sends to the webview comes from
one tested place in `ss-client-core`.

Source: T-095 added `ss_client_core::authority` (server permissions, role
rank, moderation rights) and moved `voice_view` onto it. Other shell
modules still derive permissions themselves: `role_admin::snapshot`
(administrator bit, highest position, which roles and members are
editable), `stream::can_publish`, the `can_send`/`can_attach`/`can_manage`
flags in `chat.rs`, and `channel_admin`'s manage check.

## Scope
- Add what those need to `authority` (role editability, a channel
  permission check on top of `State::permissions`) and use it from the
  shell. Keep the bridge types unchanged.

## Out of scope
Server-side authorization (T-090).

## Acceptance criteria
- Unit tests in `ss-client-core` cover each moved rule; the existing desktop
  tests still pass unchanged.

## Notes from implementation
