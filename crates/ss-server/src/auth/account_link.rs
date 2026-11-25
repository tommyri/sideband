//! Bind an existing authenticated human account to one profile in place.
use ring::signature::{ED25519, UnparsedPublicKey};
use sqlx::{Connection, SqliteConnection};
use ss_core::{
    Id, Timestamp,
    profile_link::{AccountReview, LinkAccount, transcript},
};

use super::{Auth, Authenticated, Error, db, load_person, session_alive};

impl Auth {
    pub(crate) async fn review_account(
        &self,
        account: &Authenticated,
        device: &[u8; 32],
    ) -> Result<AccountReview, Error> {
        let mut guard = self.pool.read().await?;
        let mut tx = guard.begin().await.map_err(db::Error::from)?;
        let key = membership_for(&mut tx, account.user.id).await?;
        let device_id = active_device(&mut tx, key.as_deref(), device).await?;
        let session_device: Option<Id> = sqlx::query_scalar(
            "SELECT profile_device_id FROM sessions WHERE id = ? AND user_id = ?",
        )
        .bind(account.session_id)
        .bind(account.user.id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db::Error::from)?
        .ok_or(Error::Unauthenticated)?;
        let role = db::roles::roles_for_user(&mut tx, account.user.id)
            .await?
            .pop()
            .map_or_else(|| "Member".into(), |role| role.name);
        Ok(AccountReview {
            member_id: account.user.id,
            session_id: account.session_id,
            username: account.user.username.clone(),
            role,
            membership_key: key,
            device_active: device_id.is_some(),
            device_session: device_id.is_some() && device_id == session_device,
        })
    }

    pub(crate) async fn link_account(
        &self,
        account: &Authenticated,
        proof: &LinkAccount,
    ) -> Result<(), Error> {
        if proof.membership_key == proof.device_key {
            return Err(Error::InvalidProfile);
        }
        let message = transcript(
            account.user.id,
            account.session_id,
            &proof.membership_key,
            &proof.device_key,
        );
        for (key, signature) in [
            (&proof.membership_key, &proof.membership_signature),
            (&proof.device_key, &proof.device_signature),
        ] {
            UnparsedPublicKey::new(&ED25519, key)
                .verify(&message, signature)
                .map_err(|_| Error::InvalidProfile)?;
        }
        let mut guard = self.pool.write().await;
        let mut tx = guard.begin().await.map_err(db::Error::from)?;
        let session = db::sessions::get(&mut tx, account.session_id)
            .await?
            .ok_or(Error::Unauthenticated)?;
        if session.user_id != account.user.id || !session_alive(&session, Timestamp::now()) {
            return Err(Error::Unauthenticated);
        }
        load_person(&mut tx, account.user.id).await?;
        let existing = membership_for(&mut tx, account.user.id).await?;
        let device = if let Some(key) = existing {
            if key != proof.membership_key {
                return Err(Error::InvalidProfile);
            }
            // A membership can only acquire its first device through linking.
            // Existing and revoked credentials follow normal enrollment rules.
            active_device(&mut tx, Some(&key), &proof.device_key)
                .await?
                .ok_or(Error::InvalidProfile)?
        } else {
            sqlx::query("INSERT INTO profile_memberships (membership_key,user_id) VALUES (?,?)")
                .bind(proof.membership_key.as_slice())
                .bind(account.user.id)
                .execute(&mut *tx)
                .await
                .map_err(link_error)?;
            let device = Id::generate();
            sqlx::query("INSERT INTO profile_devices (id,device_key,membership_key,device_name) VALUES (?,?,?,?)")
                .bind(device).bind(proof.device_key.as_slice()).bind(proof.membership_key.as_slice()).bind(session.device_name)
                .execute(&mut *tx).await.map_err(link_error)?;
            device
        };
        let updated=sqlx::query("UPDATE sessions SET profile_device_id=? WHERE id=? AND (profile_device_id IS NULL OR profile_device_id=?)")
            .bind(device).bind(session.id).bind(device).execute(&mut *tx).await.map_err(db::Error::from)?;
        if updated.rows_affected() != 1 {
            return Err(Error::InvalidProfile);
        }
        tx.commit().await.map_err(db::Error::from)?;
        Ok(())
    }
}

fn link_error(error: sqlx::Error) -> Error {
    match db::Error::from(error) {
        db::Error::Conflict => Error::InvalidProfile,
        other => other.into(),
    }
}

async fn membership_for(conn: &mut SqliteConnection, user: Id) -> Result<Option<Vec<u8>>, Error> {
    Ok(
        sqlx::query_scalar("SELECT membership_key FROM profile_memberships WHERE user_id=?")
            .bind(user)
            .fetch_optional(conn)
            .await
            .map_err(db::Error::from)?,
    )
}

async fn active_device(
    conn: &mut SqliteConnection,
    membership: Option<&[u8]>,
    key: &[u8; 32],
) -> Result<Option<Id>, Error> {
    Ok(sqlx::query_scalar("SELECT id FROM profile_devices WHERE membership_key=? AND device_key=? AND revoked_at IS NULL")
        .bind(membership).bind(key.as_slice()).fetch_optional(conn).await.map_err(db::Error::from)?)
}
