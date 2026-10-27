//! `roles` and `user_roles` rows.
//!
//! [`EVERYONE_ROLE_ID`] and [`ADMIN_ROLE_ID`] are the rows seeded by the
//! initial migration. `@everyone` is the default role at position 0. `Admin`
//! carries [`ss_core::Permissions::ADMINISTRATOR`] at position 1.

use sqlx::SqliteConnection;
use ss_core::{Id, Permissions, Timestamp};

use super::{
    Error,
    convert::{changed, flag, permissions_from_i64, permissions_to_i64, stamp},
    db,
    model::Role,
};

/// Seeded `@everyone` role.
pub const EVERYONE_ROLE_ID: Id = Id::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]);

/// Seeded `Admin` role.
pub const ADMIN_ROLE_ID: Id = Id::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2]);

/// Fields stored on insert. `is_default` is false. The repository sets `created_at`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewRole {
    /// Primary key.
    pub id: Id,
    /// Display name.
    pub name: String,
    /// Optional color.
    pub color: Option<i64>,
    /// Permission bits.
    pub permissions: Permissions,
    /// Hierarchy position.
    pub position: i64,
}

/// Mutable role fields. `is_default` is left unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleUpdate {
    /// Display name.
    pub name: String,
    /// Optional color.
    pub color: Option<i64>,
    /// Permission bits.
    pub permissions: Permissions,
    /// Hierarchy position.
    pub position: i64,
}

const BY_ID: &str =
    "SELECT id, name, color, permissions, position, is_default, created_at FROM roles WHERE id = ?";
const FOR_USER: &str = "SELECT id, name, color, permissions, position, is_default, created_at FROM roles WHERE id IN (SELECT role_id FROM user_roles WHERE user_id = ?) ORDER BY position, id";
const ALL: &str = "SELECT id, name, color, permissions, position, is_default, created_at FROM roles ORDER BY position, id";

/// Inserts a non-default role.
pub async fn insert(conn: &mut SqliteConnection, role: NewRole) -> Result<Role, Error> {
    let permissions = permissions_to_i64(role.permissions)?;
    let created_at = Timestamp::now().as_millis();
    db(sqlx::query(
        "INSERT INTO roles (id, name, color, permissions, position, is_default, created_at)
         VALUES (?, ?, ?, ?, ?, 0, ?)",
    )
    .bind(role.id)
    .bind(&role.name)
    .bind(role.color)
    .bind(permissions)
    .bind(role.position)
    .bind(created_at)
    .execute(&mut *conn)
    .await)?;
    get(conn, role.id).await?.ok_or(Error::NotFound)
}

/// Loads a role by id.
pub async fn get(conn: &mut SqliteConnection, id: Id) -> Result<Option<Role>, Error> {
    let row = db(sqlx::query_as::<_, RoleRow>(BY_ID)
        .bind(id)
        .fetch_optional(&mut *conn)
        .await)?;
    row.map(RoleRow::into_role).transpose()
}

/// Replaces the mutable fields of a role.
pub async fn update(conn: &mut SqliteConnection, id: Id, role: RoleUpdate) -> Result<Role, Error> {
    let permissions = permissions_to_i64(role.permissions)?;
    changed(db(sqlx::query(
        "UPDATE roles SET name = ?, color = ?, permissions = ?, position = ? WHERE id = ?",
    )
    .bind(&role.name)
    .bind(role.color)
    .bind(permissions)
    .bind(role.position)
    .bind(id)
    .execute(&mut *conn)
    .await)?)?;
    get(conn, id).await?.ok_or(Error::NotFound)
}

/// Deletes a role. Assigned roles fail the foreign key.
pub async fn delete(conn: &mut SqliteConnection, id: Id) -> Result<(), Error> {
    changed(db(sqlx::query("DELETE FROM roles WHERE id = ?")
        .bind(id)
        .execute(&mut *conn)
        .await)?)
}

/// Grants a role to a user. Granting it twice is a conflict.
pub async fn assign(conn: &mut SqliteConnection, user_id: Id, role_id: Id) -> Result<(), Error> {
    db(
        sqlx::query("INSERT INTO user_roles (user_id, role_id) VALUES (?, ?)")
            .bind(user_id)
            .bind(role_id)
            .execute(&mut *conn)
            .await,
    )?;
    Ok(())
}

/// Removes a role grant.
pub async fn unassign(conn: &mut SqliteConnection, user_id: Id, role_id: Id) -> Result<(), Error> {
    changed(db(sqlx::query(
        "DELETE FROM user_roles WHERE user_id = ? AND role_id = ?",
    )
    .bind(user_id)
    .bind(role_id)
    .execute(&mut *conn)
    .await)?)
}

/// Roles granted to a user, lowest position first.
pub async fn roles_for_user(conn: &mut SqliteConnection, user_id: Id) -> Result<Vec<Role>, Error> {
    let rows = db(sqlx::query_as::<_, RoleRow>(FOR_USER)
        .bind(user_id)
        .fetch_all(&mut *conn)
        .await)?;
    rows.into_iter().map(RoleRow::into_role).collect()
}

/// Every role, lowest position first. Equal positions break ties by id.
pub async fn list(conn: &mut SqliteConnection) -> Result<Vec<Role>, Error> {
    let rows = db(sqlx::query_as::<_, RoleRow>(ALL)
        .fetch_all(&mut *conn)
        .await)?;
    rows.into_iter().map(RoleRow::into_role).collect()
}

/// Every `(user_id, role_id)` grant.
pub async fn grants(conn: &mut SqliteConnection) -> Result<Vec<(Id, Id)>, Error> {
    let rows = db(
        sqlx::query_as::<_, GrantRow>("SELECT user_id, role_id FROM user_roles")
            .fetch_all(&mut *conn)
            .await,
    )?;
    Ok(rows
        .into_iter()
        .map(|row| (row.user_id, row.role_id))
        .collect())
}

/// Removes every grant of `role_id`. Zero rows is success.
///
/// [`delete`] fails the foreign key while any grant remains. Callers that
/// delete a role clear the grants first.
pub async fn clear_assignments(conn: &mut SqliteConnection, role_id: Id) -> Result<(), Error> {
    db(sqlx::query("DELETE FROM user_roles WHERE role_id = ?")
        .bind(role_id)
        .execute(&mut *conn)
        .await)?;
    Ok(())
}

#[derive(Debug, sqlx::FromRow)]
struct GrantRow {
    user_id: Id,
    role_id: Id,
}

#[derive(Debug, sqlx::FromRow)]
struct RoleRow {
    id: Id,
    name: String,
    color: Option<i64>,
    permissions: i64,
    position: i64,
    is_default: i64,
    created_at: i64,
}

impl RoleRow {
    fn into_role(self) -> Result<Role, Error> {
        Ok(Role {
            id: self.id,
            name: self.name,
            color: self.color,
            permissions: permissions_from_i64(self.permissions)?,
            position: self.position,
            is_default: flag(self.is_default)?,
            created_at: stamp(self.created_at),
        })
    }
}

#[cfg(test)]
mod tests {
    use ss_core::Permissions;

    use super::{
        ADMIN_ROLE_ID, EVERYONE_ROLE_ID, NewRole, RoleUpdate, assign, delete, get, insert,
        roles_for_user, unassign, update,
    };
    use crate::db::testing;

    #[tokio::test]
    async fn seeds_match_core_and_grants_round_trip() {
        let db = testing::pool().await;
        let mut conn = db.write().await;
        let everyone = get(&mut conn, EVERYONE_ROLE_ID).await.unwrap().unwrap();
        let admin = get(&mut conn, ADMIN_ROLE_ID).await.unwrap().unwrap();
        assert!(everyone.is_default);
        assert_eq!(everyone.permissions, Permissions::EVERYONE);
        assert_eq!(everyone.position, 0);
        assert!(!admin.is_default);
        assert_eq!(admin.permissions, Permissions::ADMIN);
        assert_eq!(admin.position, 1);

        let user = testing::user(&mut conn, 1).await;
        let custom = insert(
            &mut conn,
            NewRole {
                id: testing::id(3),
                name: "Mod".into(),
                color: Some(0x0033_6699),
                permissions: Permissions::MUTE_MEMBERS,
                position: 2,
            },
        )
        .await
        .unwrap();
        let renamed = update(
            &mut conn,
            custom.id,
            RoleUpdate {
                name: "Moderator".into(),
                color: None,
                permissions: Permissions::MUTE_MEMBERS,
                position: 2,
            },
        )
        .await
        .unwrap();
        assert_eq!(renamed.name, "Moderator");
        assert!(renamed.color.is_none());
        assign(&mut conn, user.id, custom.id).await.unwrap();
        assign(&mut conn, user.id, EVERYONE_ROLE_ID).await.unwrap();
        let granted = roles_for_user(&mut conn, user.id).await.unwrap();
        assert_eq!(
            granted.iter().map(|role| role.id).collect::<Vec<_>>(),
            vec![EVERYONE_ROLE_ID, custom.id]
        );
        unassign(&mut conn, user.id, custom.id).await.unwrap();
        delete(&mut conn, custom.id).await.unwrap();
        assert!(get(&mut conn, custom.id).await.unwrap().is_none());
    }
}
