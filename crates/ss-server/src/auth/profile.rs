//! First-device enrollment and sign-in for server-specific profile memberships.

use std::net::IpAddr;

use ring::signature::{ED25519, UnparsedPublicKey};
use sqlx::{Connection, SqliteConnection};
use ss_core::{Id, Timestamp};
use ss_proto::{profile, v1};

use super::{
    Auth, Authenticated, Error, classify, db, device_name, insert_member, issue_session,
    load_person, users, validate_username,
};

impl Auth {
    /// Authenticates a device on the connection identified by its TLS exporter.
    /// The boolean is true exactly when a new local member was committed.
    pub(crate) async fn profile_hello(
        &self,
        hello: &v1::Hello,
        auth: &v1::ProfileAuth,
        peer: IpAddr,
        binding: &[u8; 32],
    ) -> Result<(Authenticated, bool), Error> {
        self.limit(peer)?;
        if auth.membership_key.len() != 32
            || auth.device_key.len() != 32
            || auth.device_key == auth.membership_key
        {
            return Err(Error::InvalidProfile);
        }
        verify(
            &auth.device_key,
            &profile::login_proof(binding, &auth.membership_key, &auth.device_key),
            &auth.device_signature,
        )?;
        if let Some(register) = &auth.register {
            verify(
                &auth.membership_key,
                &profile::device_authorization(&auth.device_key),
                &register.membership_signature,
            )?;
        }
        let name = device_name(hello);
        let mut guard = self.pool.write().await;
        let mut tx = guard.begin().await.map_err(db::Error::from)?;
        let recovery = match &hello.auth {
            Some(v1::hello::Auth::RecoverProfile(recover)) => Some(recover),
            _ => None,
        };
        let existing: Option<(Id, Option<Id>, Option<i64>)> = sqlx::query_as(
            "SELECT m.user_id, d.id, d.revoked_at FROM profile_memberships m
             LEFT JOIN profile_devices d ON d.membership_key = m.membership_key
               AND d.device_key = ?
             WHERE m.membership_key = ?",
        )
        .bind(auth.device_key.as_ref())
        .bind(auth.membership_key.as_ref())
        .fetch_optional(&mut *tx)
        .await
        .map_err(db::Error::from)?;
        if let Some(recover) = recovery
            && existing.is_none_or(|(user, _, _)| recover.user_id.as_ref() != user.as_bytes())
        {
            return Err(Error::InvalidProfile);
        }
        let (user, device, created) = match existing {
            Some((user, Some(device), None)) => (load_person(&mut tx, user).await?, device, false),
            Some((user, None, None)) => {
                let recover = recovery.ok_or(Error::InvalidProfile)?;
                let member = load_person(&mut tx, user).await?;
                // The source can be offline. A revoked source cannot authorize a new device.
                verify_recovery(&mut tx, auth, recover).await?;
                let device = insert_device(&mut tx, auth, &name).await?;
                (member, device, false)
            }
            Some(_) => return Err(Error::InvalidProfile),
            None => {
                let register = auth.register.as_ref().ok_or(Error::InvalidProfile)?;
                let (user, device) = enroll(&mut tx, auth, register, &name).await?;
                (user, device, true)
            }
        };
        let authed = issue_session(&mut tx, user, name).await?;
        sqlx::query("UPDATE sessions SET profile_device_id = ? WHERE id = ?")
            .bind(device)
            .bind(authed.session_id)
            .execute(&mut *tx)
            .await
            .map_err(db::Error::from)?;
        tx.commit().await.map_err(db::Error::from)?;
        Ok((authed, created))
    }
}

fn verify(key: &[u8], message: &[u8], signature: &[u8]) -> Result<(), Error> {
    UnparsedPublicKey::new(&ED25519, key)
        .verify(message, signature)
        .map_err(|_| Error::InvalidProfile)
}

async fn verify_recovery(
    conn: &mut SqliteConnection,
    auth: &v1::ProfileAuth,
    recover: &v1::RecoverProfile,
) -> Result<(), Error> {
    // profile_hello already validated this membership signature on the new key.
    auth.register.as_ref().ok_or(Error::InvalidProfile)?;
    let transcript = profile::recovery_authorization(&auth.membership_key, &recover.user_id);
    let grant = recover.grant.as_ref().ok_or(Error::InvalidProfile)?;
    verify(&grant.source, &transcript, &grant.authorization)?;
    let active: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM profile_devices WHERE membership_key = ? AND device_key = ? AND revoked_at IS NULL)"
    ).bind(auth.membership_key.as_ref()).bind(grant.source.as_ref()).fetch_one(&mut *conn).await.map_err(db::Error::from)?;
    if active {
        Ok(())
    } else {
        Err(Error::InvalidProfile)
    }
}

async fn enroll(
    conn: &mut SqliteConnection,
    auth: &v1::ProfileAuth,
    register: &v1::ProfileRegister,
    device_name: &str,
) -> Result<(db::User, Id), Error> {
    validate_username(&register.username)?;
    let kind = classify(conn, &register.invite_code, Timestamp::now()).await?;
    // An empty password hash is the explicit passwordless marker.
    let user = insert_member(
        conn,
        &register.username,
        String::new(),
        kind,
        &register.invite_code,
    )
    .await?;
    sqlx::query("INSERT INTO profile_memberships (membership_key, user_id) VALUES (?, ?)")
        .bind(auth.membership_key.as_ref())
        .bind(user.id)
        .execute(&mut *conn)
        .await
        .map_err(db::Error::from)?;
    let device = insert_device(conn, auth, device_name).await?;
    Ok((user, device))
}

async fn insert_device(
    conn: &mut SqliteConnection,
    auth: &v1::ProfileAuth,
    device_name: &str,
) -> Result<Id, Error> {
    let device = Id::generate();
    sqlx::query(
        "INSERT INTO profile_devices (id, device_key, membership_key, device_name) VALUES (?, ?, ?, ?)",
    )
    .bind(device)
    .bind(auth.device_key.as_ref())
    .bind(auth.membership_key.as_ref())
    .bind(device_name)
    .execute(&mut *conn)
    .await
    .map_err(|error| match db::Error::from(error) {
        db::Error::Conflict => Error::InvalidProfile,
        other => other.into(),
    })?;
    Ok(device)
}

/// A currently enrolled device on a local membership.
pub struct ProfileDevice {
    /// Local operator-facing id for revocation.
    pub id: Id,
    /// Client name recorded at enrollment.
    pub name: String,
}

/// Lists the active profile device credentials for one account.
pub async fn list_profile_devices(
    pool: &db::Pool,
    username: &str,
) -> Result<Vec<ProfileDevice>, Error> {
    let mut conn = pool.read().await?;
    let user = users::get_by_username(&mut conn, username)
        .await?
        .ok_or(Error::NotFound)?;
    let rows: Vec<(Id, String)> = sqlx::query_as(
        "SELECT d.id, d.device_name FROM profile_devices d
         JOIN profile_memberships m ON m.membership_key = d.membership_key
         WHERE m.user_id = ? AND d.revoked_at IS NULL ORDER BY d.id",
    )
    .bind(user.id)
    .fetch_all(&mut *conn)
    .await
    .map_err(db::Error::from)?;
    Ok(rows
        .into_iter()
        .map(|(id, name)| ProfileDevice { id, name })
        .collect())
}

/// Revokes one enrolled device and invalidates every session it issued.
/// The enrollment tombstone prevents its key from being registered again.
pub async fn revoke_profile_device(
    pool: &db::Pool,
    username: &str,
    device: Id,
) -> Result<u64, Error> {
    let mut guard = pool.write().await;
    let mut tx = guard.begin().await.map_err(db::Error::from)?;
    let user = users::get_by_username(&mut tx, username)
        .await?
        .ok_or(Error::NotFound)?;
    let result = sqlx::query(
        "UPDATE profile_devices SET revoked_at = COALESCE(revoked_at, ?) WHERE id = ?
         AND membership_key = (SELECT membership_key FROM profile_memberships WHERE user_id = ?)",
    )
    .bind(Timestamp::now().as_millis())
    .bind(device)
    .bind(user.id)
    .execute(&mut *tx)
    .await
    .map_err(db::Error::from)?;
    if result.rows_affected() == 0 {
        return Err(Error::NotFound);
    }
    let ended = sqlx::query("DELETE FROM sessions WHERE profile_device_id = ?")
        .bind(device)
        .execute(&mut *tx)
        .await
        .map_err(db::Error::from)?
        .rows_affected();
    tx.commit().await.map_err(db::Error::from)?;
    Ok(ended)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use ring::signature::{Ed25519KeyPair, KeyPair};

    use super::*;

    #[tokio::test]
    async fn enrollment_requires_the_current_connection_and_membership_approval() {
        let dir = tempfile::tempdir().unwrap();
        let pool = Arc::new(db::Pool::open(dir.path().join("test.db")).await.unwrap());
        let code = super::super::ensure_owner_invite(&pool)
            .await
            .unwrap()
            .unwrap();
        let auth = Auth::new(pool.clone());
        let hello = v1::Hello::default();
        let peer = IpAddr::from([192, 0, 2, 1]);
        let membership = Ed25519KeyPair::from_seed_unchecked(&[1; 32]).unwrap();
        let device = Ed25519KeyPair::from_seed_unchecked(&[2; 32]).unwrap();
        let membership_key = membership.public_key().as_ref();
        let device_key = device.public_key().as_ref();
        let binding = [3; 32];
        let mut credential = v1::ProfileAuth {
            membership_key: membership_key.to_vec().into(),
            device_key: device_key.to_vec().into(),
            device_signature: device
                .sign(&profile::login_proof(&binding, membership_key, device_key))
                .as_ref()
                .to_vec()
                .into(),
            register: Some(v1::ProfileRegister {
                username: "first".to_owned(),
                invite_code: code,
                membership_signature: membership
                    .sign(&profile::device_authorization(device_key))
                    .as_ref()
                    .to_vec()
                    .into(),
            }),
        };
        assert!(matches!(
            auth.profile_hello(&hello, &credential, peer, &[4; 32])
                .await,
            Err(Error::InvalidProfile)
        ));
        let mut unapproved = credential.clone();
        unapproved.register.as_mut().unwrap().membership_signature = bytes::Bytes::new();
        assert!(matches!(
            auth.profile_hello(&hello, &unapproved, peer, &binding)
                .await,
            Err(Error::InvalidProfile)
        ));
        let (created, new) = auth
            .profile_hello(&hello, &credential, peer, &binding)
            .await
            .unwrap();
        assert!(new);
        credential.register = None;
        let (returning, new) = auth
            .profile_hello(&hello, &credential, peer, &binding)
            .await
            .unwrap();
        assert!(!new);
        assert_eq!(created.user.id, returning.user.id);
    }
}
