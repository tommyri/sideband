# T-105 — Remaining permission projections use ss-client-core authority

Status: in-progress
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

Implemented 2026-11-11. Tests were written first in
`ss_client_core::authority` from the shell's behaviour and seen failing to
compile, then passing on the moved code. Where the shell and the server
(T-090) disagreed, the test follows the server and is marked in a comment.

**Where each rule lives now** (all in `crates/ss-client-core/src/authority.rs`)

| Shell rule | Now |
|---|---|
| `role_admin::snapshot` administrator bit, `manage_roles`, `manage_channels` | `Authority::is_administrator`, `allows(MANAGE_ROLES)`, `can_manage_channels` |
| Highest position and `create_position` | `Authority::new_role_position` (bottom for an administrator, else one below the top role, `None` at `i32::MIN`) |
| `AdminRole.editable` | `Authority::can_edit_role`: administrator, or below the top role without `ADMINISTRATOR` (the server's `ensure_can_edit`). It deliberately does not require `MANAGE_ROLES`: the same flag gates channel overrides, which need `MANAGE_CHANNELS`. |
| `RoleMember.editable` | `Authority::can_set_roles_of`: `MANAGE_ROLES` and the server's `can_act_on` |
| `stream::can_publish` | `authority::can_publish` (the shell imports it; its existing test is unchanged) |
| `chat.rs` `can_send` / `can_attach` / `can_manage` and the visibility error | `authority::ChatRights::of` (`None` when the channel is not visible) |
| `channel_admin::authorize` | `Authority::can_manage_channels`, read directly instead of building a whole `voice_view::snapshot` (which also read storage) |
| Channel check on `State::permissions` | `authority::holds_in` |

Unknown permission bits (T-101) are read through `from_bits_truncate`
everywhere, so a role mask from a newer server is judged by its known bits
(`a_role_from_a_newer_server_is_judged_by_its_known_bits`). The save path
(`role_admin::mask`) is untouched; it still rejects unknown bits (T-108).

**Shell and server disagreements, now following the server**
- A non-administrator whose top role is above an administrator's was offered
  the administrator's roles (`RoleMember.editable`) and, through T-095's
  `Authority::outranks`, kick, ban, mute and move. The server's `can_act_on`
  refuses both since T-090. `outranks` and `can_set_roles_of` now share the
  server rule (`only_administrators_act_on_administrators`,
  `member_roles_need_manage_roles_and_a_higher_top_role`). This changes the
  moderation menu in `voice_view` too.
- `can_attach` was `ATTACH_FILES` alone; the server checks `SEND_MESSAGES`
  first, so it is now both (`attaching_also_needs_send_messages`). The
  composer disables attaching for such a user.
- `can_publish` required `CONNECT`; the server's `StartStream` checks only a
  seat in the channel, `VIEW_CHANNEL` and `STREAM`, and a member moved in by
  a moderator keeps the seat without `CONNECT`. Such a member is now offered
  screen share (`a_moved_member_without_connect_may_still_share`).

Agreed already, left as is: role editability and the create position match
`ensure_can_edit`/`ensure_position`; an administrator may set their own
roles, as `can_act_on` allows. One remaining, deliberate difference:
moderation never offers acting on yourself, while the server lets an
administrator server-mute themselves through `can_act_on`; kick, ban and move
refuse self on the server too.

**Bridge types** are unchanged (no ts-rs output changed); only the values of
`RoleMember.editable`, `ChatSnapshot.canAttach` and the screen-share
permission follow the server in the cases above. Existing desktop tests pass
unchanged. `stream.rs` imports `Permissions` under `#[cfg(test)]` only,
because its unchanged test uses it.

Status stays `in-progress` until T-096 is marked merged: `plan-check` does
not allow `done` while a dependency is `in-progress`. No code or manual
check is left in this task's scope.

**Testing.** `just check` (nightly rustfmt, clippy, workspace tests,
plan-check); `cargo clippy -p socialspeak-desktop --all-targets -- -D
warnings`; `cargo test -p socialspeak-desktop`.

**Files outside the Paths:** this task file and `INDEX.md`.

**Follow-up for T-108:** `voice_view::snapshot` still computes
`can_manage_channels` as `allows(MANAGE_CHANNELS)`; it could call
`Authority::can_manage_channels` (same rule, outside these Paths).
