# T-022 — Permissions engine and role management

Status: done
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

- `PermissionService` holds an `ArcSwap` snapshot of roles, grants, channels, members, and overrides. Handlers reload it after the write commits and before they release the write lock, so the next check in this process sees the change. `check` itself is synchronous.
- Hello still stores the OR of the user's roles on the connection. Command checks do not read that copy. There is no owner column: the owner holds the seeded Admin role, and `ADMINISTRATOR` is the hierarchy short-circuit (`can_act_on` is always true, and only an administrator may grant that bit).
- `CreateRole`, `UpdateRole`, `DeleteRole`, and `SetUserRoles` require `MANAGE_ROLES`. `SetChannelOverride` requires `MANAGE_CHANNELS` (bit 2), which is the bit the data model assigns to overrides.
- A non-admin can only create, edit, grant, or override a role whose position is strictly below their highest role. Seeded Admin is position 1, so a role placed above it outranks the admin user unless the actor holds `ADMINISTRATOR`. `@everyone` cannot be deleted. Delete clears grants and overrides, then rewrites the remaining positions to `0..n-1` in `(position, id)` order. Create stores the requested position and does not compact.
- Direct channels ignore roles, overrides, and `ADMINISTRATOR`. Visibility is membership only, which is `Permissions::resolve`.
- Each mutation writes one `audit_log` row. The connection sends the Ack for the request id, then `RoleCreated`, `RoleUpdated`, `RoleDeleted`, or `MemberRolesUpdated` with request id 0. There is no fan-out yet and no override event. The voice-spike accept loop is unchanged.
