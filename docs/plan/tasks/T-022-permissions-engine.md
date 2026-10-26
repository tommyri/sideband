# T-022 — Permissions engine and role management

Status: todo
Phase: 2
Size: M
Depends on: T-020
Paths: `crates/ss-server/src/perms.rs`, `crates/ss-server/src/handlers/roles.rs`

## Goal
One place that answers "may user U do X in channel C?" using
`ss_core::Permissions::resolve`, and the role CRUD commands that feed it.

## Scope
- `PermissionService`: loads roles, user_roles, channel_overrides into an
  in-memory cache (`ArcSwap`) invalidated on mutation; `check(user, channel?,
  perm) -> Result<(), Forbidden>`; `highest_role_position(user)`;
  `can_act_on(actor, target)` hierarchy rule; owner short-circuit.
- Handlers: `CreateRole`, `UpdateRole`, `DeleteRole`, `SetUserRoles`,
  `SetChannelOverride` with: `MANAGE_ROLES`, hierarchy checks (cannot create or
  grant a role at/above own highest; cannot grant `ADMINISTRATOR` unless
  owner/admin), cannot delete `@everyone`, position compaction.
- Events: `RoleCreated/Updated/Deleted`, `MemberRolesUpdated`.
- Audit log entries for every mutation.
- Visibility helper: `visible_channels(user) -> Set<ChannelId>` used by the
  snapshot and event filters.

## Out of scope
UI, channel CRUD (T-023).

## Acceptance criteria
- `tests/permissions.rs`: table-driven matrix (actor roles × target × command
  → expected) covering hierarchy, admin bypass, overrides, DM rule, `@everyone`
  protection.
- Cache invalidation test: change a role and the next check reflects it
  without restart.

## Notes from implementation
