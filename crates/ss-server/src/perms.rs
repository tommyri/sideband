//! Answers whether a user may do something, in a channel or on the server.
//!
//! [`PermissionService`] keeps an [`ArcSwap`] snapshot of roles, grants, and
//! channel overrides. Handlers call [`PermissionService::reload`] after a
//! committed mutation so the next check sees it. DM handlers call
//! [`PermissionService::set_direct`] instead: roles and overrides never apply
//! to a DM, so its member list is all that changes, and a full reload would
//! read every role, channel and membership on the server. `Hello` still
//! records the role OR on the connection; command checks do not read that
//! copy.

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use arc_swap::ArcSwap;
use ss_core::{ChannelKind, Id, PermissionOverride, Permissions};

use crate::db::{self, EVERYONE_ROLE_ID, channels, roles};

/// Why a permission check failed.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum AccessError {
    /// The user does not hold the bit.
    #[error("forbidden")]
    Forbidden,
    /// The channel is missing or soft-deleted.
    #[error("not found")]
    NotFound,
}

/// Cached roles, grants, and channel overrides for one process.
pub struct PermissionService {
    pool: Arc<db::Pool>,
    cache: ArcSwap<Snapshot>,
    patches: parking_lot::Mutex<Patches>,
}

/// DM patches a reload in flight must re-apply. A reload reads the database
/// without a lock, so a DM change committed and patched after that read
/// began would otherwise be overwritten when the reload stores, and a member
/// who left would regain access until the next reload. Every store goes
/// through this lock; the database read does not.
#[derive(Default)]
struct Patches {
    /// Bumped by every [`PermissionService::set_direct`].
    generation: u64,
    /// Reloads in flight, by the generation they started at.
    active: std::collections::BTreeMap<u64, usize>,
    /// Patches newer than the oldest reload in flight, in order.
    log: Vec<(u64, Id, Arc<ChannelView>)>,
}

#[derive(Clone, Copy)]
struct RoleView {
    permissions: Permissions,
    position: i64,
}

struct ChannelView {
    kind: ChannelKind,
    members: HashSet<Id>,
    overrides: HashMap<Id, PermissionOverride>,
}

/// The patch generation a reload started at.
#[derive(Clone, Copy)]
struct ReloadStart(u64);

/// Roles and grants are shared, and each channel is its own allocation, so
/// [`PermissionService::set_direct`] copies pointers rather than the whole
/// snapshot.
#[derive(Clone)]
struct Snapshot {
    roles: Arc<HashMap<Id, RoleView>>,
    grants: Arc<HashMap<Id, Vec<Id>>>,
    channels: HashMap<Id, Arc<ChannelView>>,
}

impl PermissionService {
    /// Loads the snapshot from `pool`.
    pub async fn open(pool: Arc<db::Pool>) -> Result<Self, db::Error> {
        let snapshot = load(&pool).await?;
        Ok(Self {
            pool,
            cache: ArcSwap::from_pointee(snapshot),
            patches: parking_lot::Mutex::new(Patches::default()),
        })
    }

    /// The database the snapshot was loaded from.
    pub(crate) fn pool(&self) -> &db::Pool {
        &self.pool
    }

    /// Replaces the snapshot. Call this after a role or override commit.
    pub async fn reload(&self) -> Result<(), db::Error> {
        let started = self.begin_reload();
        let snapshot = load(&self.pool).await;
        self.finish_reload(started, snapshot)
    }

    fn begin_reload(&self) -> ReloadStart {
        let mut patches = self.patches.lock();
        let generation = patches.generation;
        *patches.active.entry(generation).or_default() += 1;
        ReloadStart(generation)
    }

    /// Stores a loaded snapshot with every DM patch made since its read
    /// began applied on top, then forgets patches no reload still needs.
    fn finish_reload(
        &self,
        started: ReloadStart,
        snapshot: Result<Snapshot, db::Error>,
    ) -> Result<(), db::Error> {
        let mut patches = self.patches.lock();
        if let Some(count) = patches.active.get_mut(&started.0) {
            *count -= 1;
            if *count == 0 {
                patches.active.remove(&started.0);
            }
        }
        let result = snapshot.map(|mut snapshot| {
            for (generation, channel, view) in &patches.log {
                if *generation > started.0 {
                    snapshot.channels.insert(*channel, Arc::clone(view));
                }
            }
            self.cache.store(Arc::new(snapshot));
        });
        match patches.active.keys().next().copied() {
            Some(oldest) => patches.log.retain(|(generation, ..)| *generation > oldest),
            None => patches.log.clear(),
        }
        result
    }

    /// Records the current members of DM `channel` without reloading. Call
    /// it after committing a DM create, add, or leave. A reload whose read
    /// began before this call re-applies it when it stores.
    pub fn set_direct(&self, channel: Id, kind: ChannelKind, members: &[Id]) {
        let view = Arc::new(ChannelView {
            kind,
            members: members.iter().copied().collect(),
            overrides: HashMap::new(),
        });
        let mut patches = self.patches.lock();
        patches.generation += 1;
        if !patches.active.is_empty() {
            let generation = patches.generation;
            patches.log.push((generation, channel, Arc::clone(&view)));
        }
        let mut next = Snapshot::clone(&self.cache.load());
        next.channels.insert(channel, view);
        self.cache.store(Arc::new(next));
    }

    /// Members of a live DM, ordered by id. `None` for a guild channel or a
    /// missing one.
    #[must_use]
    pub fn direct_members(&self, channel: Id) -> Option<Vec<Id>> {
        let snapshot = self.cache.load();
        let view = snapshot.channels.get(&channel)?;
        if !view.kind.is_direct() {
            return None;
        }
        let mut members: Vec<Id> = view.members.iter().copied().collect();
        members.sort_unstable();
        Some(members)
    }

    /// Whether `user` holds `permission` on the server, or in `channel`.
    ///
    /// `channel == None` is the OR of the user's roles, with `@everyone`
    /// included, and [`Permissions::all`] when a role has `ADMINISTRATOR`.
    /// A missing or soft-deleted channel is [`AccessError::NotFound`].
    pub fn check(
        &self,
        user: Id,
        channel: Option<Id>,
        permission: Permissions,
    ) -> Result<(), AccessError> {
        let snapshot = self.cache.load();
        let held = match channel {
            Some(id) => snapshot.channel_permissions(user, id)?,
            None => snapshot.server_permissions(user),
        };
        if held.contains(permission) {
            Ok(())
        } else {
            Err(AccessError::Forbidden)
        }
    }

    /// Resolved permissions for a user in a channel, including overrides.
    pub fn channel_permissions(&self, user: Id, channel: Id) -> Result<Permissions, AccessError> {
        self.cache.load().channel_permissions(user, channel)
    }

    /// Highest role position `user` holds. `@everyone` counts.
    #[must_use]
    pub fn highest_role_position(&self, user: Id) -> i64 {
        self.cache.load().highest(user)
    }

    /// Whether `actor` may change `target`.
    ///
    /// An `ADMINISTRATOR` always may, and nobody else may change an
    /// administrator, whatever the role positions. There is no separate
    /// owner: the owner holds the seeded Admin role like any other
    /// administrator, and the operator CLI is the recovery path. Otherwise
    /// the actor's highest role must strictly outrank the target's.
    #[must_use]
    pub fn can_act_on(&self, actor: Id, target: Id) -> bool {
        let snapshot = self.cache.load();
        if snapshot.is_administrator(actor) {
            return true;
        }
        if snapshot.is_administrator(target) {
            return false;
        }
        Permissions::hierarchy_allows(snapshot.highest(actor), snapshot.highest(target))
    }

    /// Whether `user` holds every bit of `permissions` on the server, or in
    /// `channel` after overrides. An empty set always holds.
    #[must_use]
    pub fn holds(&self, user: Id, channel: Option<Id>, permissions: Permissions) -> bool {
        permissions.is_empty() || self.check(user, channel, permissions).is_ok()
    }

    /// Whether `user` holds `ADMINISTRATOR` through a role.
    #[must_use]
    pub fn is_administrator(&self, user: Id) -> bool {
        self.cache.load().is_administrator(user)
    }

    /// Channels `user` can see. Direct channels require membership.
    /// Soft-deleted channels are omitted.
    #[must_use]
    pub fn visible_channels(&self, user: Id) -> HashSet<Id> {
        let snapshot = self.cache.load();
        snapshot
            .channels
            .keys()
            .copied()
            .filter(|id| {
                snapshot
                    .channel_permissions(user, *id)
                    .is_ok_and(|permissions| permissions.contains(Permissions::VIEW_CHANNEL))
            })
            .collect()
    }
}

impl Snapshot {
    fn role_ids(&self, user: Id) -> Vec<Id> {
        let mut ids = self.grants.get(&user).cloned().unwrap_or_default();
        if !ids.contains(&EVERYONE_ROLE_ID) {
            ids.push(EVERYONE_ROLE_ID);
        }
        ids.retain(|id| self.roles.contains_key(id));
        ids.sort_by_key(|id| {
            let position = self.roles.get(id).map_or(0, |role| role.position);
            (position, *id)
        });
        ids
    }

    fn server_permissions(&self, user: Id) -> Permissions {
        let mut base = Permissions::empty();
        for id in self.role_ids(user) {
            if let Some(role) = self.roles.get(&id) {
                base = base.union(role.permissions);
            }
        }
        if base.contains(Permissions::ADMINISTRATOR) {
            Permissions::all()
        } else {
            base
        }
    }

    fn is_administrator(&self, user: Id) -> bool {
        self.server_permissions(user)
            .contains(Permissions::ADMINISTRATOR)
    }

    fn highest(&self, user: Id) -> i64 {
        self.role_ids(user)
            .into_iter()
            .filter_map(|id| self.roles.get(&id).map(|role| role.position))
            .max()
            .unwrap_or(0)
    }

    fn channel_permissions(&self, user: Id, channel: Id) -> Result<Permissions, AccessError> {
        let view = self.channels.get(&channel).ok_or(AccessError::NotFound)?;
        let ids = self.role_ids(user);
        let role_permissions = ids
            .iter()
            .filter_map(|id| self.roles.get(id).map(|role| role.permissions));
        let overrides = ids.iter().map(|id| {
            view.overrides
                .get(id)
                .copied()
                .unwrap_or_else(empty_override)
        });
        Ok(Permissions::resolve(
            role_permissions,
            overrides,
            view.kind,
            view.members.contains(&user),
        ))
    }
}

const fn empty_override() -> PermissionOverride {
    PermissionOverride::new(Permissions::empty(), Permissions::empty())
}

async fn load(pool: &db::Pool) -> Result<Snapshot, db::Error> {
    let mut conn = pool.read().await?;
    let listed = roles::list(&mut conn).await?;
    let mut role_views = HashMap::with_capacity(listed.len());
    for role in listed {
        role_views.insert(
            role.id,
            RoleView {
                permissions: role.permissions,
                position: role.position,
            },
        );
    }
    let mut grants: HashMap<Id, Vec<Id>> = HashMap::new();
    for (user_id, role_id) in roles::grants(&mut conn).await? {
        grants.entry(user_id).or_default().push(role_id);
    }
    let mut channel_views = HashMap::new();
    for channel in channels::list(&mut conn).await? {
        channel_views.insert(
            channel.id,
            ChannelView {
                kind: channel.kind,
                members: HashSet::new(),
                overrides: HashMap::new(),
            },
        );
    }
    for (channel_id, user_id) in channels::memberships(&mut conn).await? {
        if let Some(view) = channel_views.get_mut(&channel_id) {
            view.members.insert(user_id);
        }
    }
    for row in channels::list_overrides(&mut conn).await? {
        if let Some(view) = channel_views.get_mut(&row.channel_id) {
            view.overrides
                .insert(row.role_id, PermissionOverride::new(row.allow, row.deny));
        }
    }
    Ok(Snapshot {
        roles: Arc::new(role_views),
        grants: Arc::new(grants),
        channels: channel_views
            .into_iter()
            .map(|(id, view)| (id, Arc::new(view)))
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use ss_core::{ChannelKind, Id, Permissions};

    use super::{PermissionService, load};
    use crate::db::{self, NewChannel, channels, testing};

    #[tokio::test]
    async fn a_reload_that_read_before_a_dm_leave_does_not_restore_access() {
        let dir = tempfile::tempdir().unwrap();
        let pool = Arc::new(db::Pool::open(dir.path().join("perms.db")).await.unwrap());
        let (ada, bea, cyd) = {
            let mut conn = pool.write().await;
            (
                testing::user(&mut conn, 1).await.id,
                testing::user(&mut conn, 2).await.id,
                testing::user(&mut conn, 3).await.id,
            )
        };
        let group = Id::generate();
        {
            let mut conn = pool.write().await;
            channels::insert(
                &mut conn,
                NewChannel {
                    id: group,
                    kind: ChannelKind::GroupDm,
                    name: String::new(),
                    topic: None,
                    parent_id: None,
                    position: 0,
                    voice_profile: None,
                    voice_bitrate: None,
                    user_limit: None,
                    created_by: ada,
                },
            )
            .await
            .unwrap();
            for member in [ada, bea, cyd] {
                channels::add_member(&mut conn, group, member)
                    .await
                    .unwrap();
            }
        }
        let perms = PermissionService::open(Arc::clone(&pool)).await.unwrap();
        let view = |user| {
            perms
                .check(user, Some(group), Permissions::VIEW_CHANNEL)
                .is_ok()
        };
        assert!(view(bea));

        // A reload (say, from someone's Hello) reads while Bea is a member...
        let started = perms.begin_reload();
        let stale = load(&pool).await;
        // ...Bea leaves and the handler patches the snapshot...
        {
            let mut conn = pool.write().await;
            channels::remove_member(&mut conn, group, bea)
                .await
                .unwrap();
        }
        perms.set_direct(group, ChannelKind::GroupDm, &[ada, cyd]);
        assert!(!view(bea));
        // ...and only then does the reload store what it read.
        perms.finish_reload(started, stale).unwrap();
        assert!(
            !view(bea),
            "a stale reload gave a departed member access back"
        );
        assert!(view(ada) && view(cyd));
    }
}
