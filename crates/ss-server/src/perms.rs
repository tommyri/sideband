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

/// Orders the stores of reloads in flight and holds the DM patches they must
/// re-apply. A reload reads the database without a lock, so two hazards
/// remain when it stores, and both are settled under this lock:
///
/// - A reload that began earlier may finish later. Its read can predate a
///   commit that a newer reload already stored, and storing it would give a
///   demoted member their old permissions until the next reload. Each reload
///   takes a ticket when it begins, and one whose ticket is older than the
///   snapshot already stored drops what it read.
/// - A DM change committed and patched after a reload's read began would be
///   overwritten when that reload stores, and a member who left would regain
///   access. The reload re-applies those patches.
///
/// Every store goes through this lock; the database read does not.
#[derive(Default)]
struct Patches {
    /// Tickets handed to reloads, in the order they began.
    tickets: u64,
    /// Ticket of the reload whose read is in the cache. Zero is `open`.
    stored: u64,
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

impl Patches {
    /// Deregisters one reload that started at `generation`, then forgets
    /// patches no remaining reload needs.
    fn release(&mut self, generation: u64) {
        if let Some(count) = self.active.get_mut(&generation) {
            *count -= 1;
            if *count == 0 {
                self.active.remove(&generation);
            }
        }
        match self.active.keys().next().copied() {
            Some(oldest) => self.log.retain(|(patched, ..)| *patched > oldest),
            None => self.log.clear(),
        }
    }
}

/// A reload in flight, registered at the patch generation it started at.
///
/// Reloads run inside command tasks, which are aborted when their connection
/// drops. Dropping this guard without [`PermissionService::finish_reload`]
/// still deregisters the reload, so an abandoned read cannot keep the patch
/// log growing for the life of the process.
struct ReloadStart<'a> {
    service: &'a PermissionService,
    /// Orders this reload against the others; see [`Patches`].
    ticket: u64,
    generation: u64,
    finished: bool,
}

impl Drop for ReloadStart<'_> {
    fn drop(&mut self) {
        if !self.finished {
            self.service.patches.lock().release(self.generation);
        }
    }
}

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
    ///
    /// Reloads may overlap (every `Hello` reloads too). The snapshot a call
    /// leaves is never older than its own read: a reload that began before
    /// this one and finishes after it does not replace it.
    pub async fn reload(&self) -> Result<(), db::Error> {
        let started = self.begin_reload();
        let snapshot = load(&self.pool).await;
        self.finish_reload(started, snapshot)
    }

    fn begin_reload(&self) -> ReloadStart<'_> {
        let mut patches = self.patches.lock();
        patches.tickets += 1;
        let ticket = patches.tickets;
        let generation = patches.generation;
        *patches.active.entry(generation).or_default() += 1;
        ReloadStart {
            service: self,
            ticket,
            generation,
            finished: false,
        }
    }

    /// Stores a loaded snapshot with every DM patch made since its read
    /// began applied on top, unless a reload that began later has already
    /// stored, then forgets patches no reload still needs.
    fn finish_reload(
        &self,
        mut started: ReloadStart<'_>,
        snapshot: Result<Snapshot, db::Error>,
    ) -> Result<(), db::Error> {
        started.finished = true;
        let mut patches = self.patches.lock();
        let result = snapshot.map(|mut snapshot| {
            // The cache already holds a read that began after this one.
            if started.ticket < patches.stored {
                return;
            }
            for (generation, channel, view) in &patches.log {
                if *generation > started.generation {
                    snapshot.channels.insert(*channel, Arc::clone(view));
                }
            }
            self.cache.store(Arc::new(snapshot));
            patches.stored = started.ticket;
        });
        patches.release(started.generation);
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
    use crate::db::{self, NewChannel, NewRole, channels, roles, testing};

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

    #[tokio::test]
    async fn an_older_reload_never_replaces_a_newer_one() {
        let dir = tempfile::tempdir().unwrap();
        let pool = Arc::new(db::Pool::open(dir.path().join("perms.db")).await.unwrap());
        let (ada, role) = {
            let mut conn = pool.write().await;
            let ada = testing::user(&mut conn, 1).await.id;
            let role = roles::insert(
                &mut conn,
                NewRole {
                    id: Id::generate(),
                    name: "Mods".into(),
                    color: None,
                    permissions: Permissions::MANAGE_CHANNELS,
                    position: 1,
                },
            )
            .await
            .unwrap();
            roles::assign(&mut conn, ada, role.id).await.unwrap();
            (ada, role.id)
        };
        let perms = PermissionService::open(Arc::clone(&pool)).await.unwrap();
        let manages = || perms.holds(ada, None, Permissions::MANAGE_CHANNELS);
        assert!(manages());

        // Someone's Hello starts a reload and reads while Ada is a moderator...
        let hello = perms.begin_reload();
        let stale = load(&pool).await;
        // ...an admin demotes her and the command reloads and stores...
        {
            let mut conn = pool.write().await;
            roles::unassign(&mut conn, ada, role).await.unwrap();
        }
        perms.reload().await.unwrap();
        assert!(!manages());
        // ...and only then does the Hello store what it read.
        perms.finish_reload(hello, stale).unwrap();
        assert!(
            !manages(),
            "an older reload restored the permissions of a demoted member"
        );
    }

    #[tokio::test]
    async fn an_abandoned_reload_does_not_keep_the_patch_log_growing() {
        let dir = tempfile::tempdir().unwrap();
        let pool = Arc::new(db::Pool::open(dir.path().join("perms.db")).await.unwrap());
        let perms = PermissionService::open(Arc::clone(&pool)).await.unwrap();
        let group = Id::generate();

        // A reload whose command task is aborted during its read...
        let started = perms.begin_reload();
        perms.set_direct(group, ChannelKind::GroupDm, &[Id::generate()]);
        assert_eq!(perms.patches.lock().log.len(), 1);
        drop(started);
        // ...is deregistered, so later DM changes are not logged forever.
        for _ in 0..3 {
            perms.set_direct(group, ChannelKind::GroupDm, &[Id::generate()]);
        }
        {
            let patches = perms.patches.lock();
            assert!(
                patches.active.is_empty(),
                "the dropped reload stayed registered"
            );
            assert!(patches.log.is_empty(), "the patch log kept growing");
        }

        // A reload cancelled by a timeout while it reads behaves the same.
        let _ = tokio::time::timeout(std::time::Duration::ZERO, perms.reload()).await;
        perms.set_direct(group, ChannelKind::GroupDm, &[Id::generate()]);
        let patches = perms.patches.lock();
        assert!(patches.active.is_empty() && patches.log.is_empty());
    }
}
