//! Answers whether a user may do something, in a channel or on the server.
//!
//! [`PermissionService`] keeps an [`ArcSwap`] snapshot of roles, grants, and
//! channel overrides. Handlers call [`PermissionService::reload`] after a
//! committed mutation so the next check sees it. `Hello` still records the
//! role OR on the connection; command checks do not read that copy.

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

struct Snapshot {
    roles: HashMap<Id, RoleView>,
    grants: HashMap<Id, Vec<Id>>,
    channels: HashMap<Id, ChannelView>,
}

impl PermissionService {
    /// Loads the snapshot from `pool`.
    pub async fn open(pool: Arc<db::Pool>) -> Result<Self, db::Error> {
        let snapshot = load(&pool).await?;
        Ok(Self {
            pool,
            cache: ArcSwap::from_pointee(snapshot),
        })
    }

    /// The database the snapshot was loaded from.
    pub(crate) fn pool(&self) -> &db::Pool {
        &self.pool
    }

    /// Replaces the snapshot. Call this after a role or override commit.
    pub async fn reload(&self) -> Result<(), db::Error> {
        let snapshot = load(&self.pool).await?;
        self.cache.store(Arc::new(snapshot));
        Ok(())
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
    /// An `ADMINISTRATOR` always may. That is the owner short-circuit: the
    /// owner holds the seeded Admin role, and there is no separate owner
    /// column. Otherwise the actor's highest role must strictly outrank the
    /// target's.
    #[must_use]
    pub fn can_act_on(&self, actor: Id, target: Id) -> bool {
        let snapshot = self.cache.load();
        if snapshot.is_administrator(actor) {
            return true;
        }
        Permissions::hierarchy_allows(snapshot.highest(actor), snapshot.highest(target))
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
        roles: role_views,
        grants,
        channels: channel_views,
    })
}
