//! Authenticated review and explicit confirmation of an existing account.
use super::ProfileCredentials;
use crate::{
    Client, Error,
    http::{HttpSession, read_bounded},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ring::signature::KeyPair;
use ss_core::{
    Id,
    profile_link::{AccountReview, LinkAccount, transcript},
};

/// Whether the server can link the signed-in account to these scoped keys.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LinkState {
    /// Ready for the member's explicit approval.
    Eligible,
    /// This session and device already belong to this profile.
    Linked,
    /// The account was linked to a different profile.
    OtherProfile,
    /// This device was never enrolled, or was revoked.
    UnauthorizedDevice,
}

/// Server-verified identity shown before the link is submitted.
pub struct ReviewedAccount {
    /// Server's stable member id.
    pub id: Id,
    /// Server login name.
    pub username: String,
    /// Current highest role.
    pub role: String,
    /// Link eligibility.
    pub state: LinkState,
}

const PATH: &str = "api/v1/me/profile-link";

impl Client {
    /// Read the current member and compare its existing binding with the local keys.
    pub async fn review_profile_link(
        &self,
        keys: &ProfileCredentials,
    ) -> Result<ReviewedAccount, Error> {
        let http = self.http_session()?;
        let review = self.link_review(&http, keys).await?;
        Ok(ReviewedAccount {
            state: keys.link_state(&review),
            id: review.member_id,
            username: review.username,
            role: review.role,
        })
    }

    /// Recheck the reviewed account and bind it in place. No password or bearer goes into a profile.
    pub async fn link_profile_account(
        &self,
        keys: &ProfileCredentials,
        member: Id,
        role: &str,
    ) -> Result<(), Error> {
        let http = self.http_session()?;
        let review = self.link_review(&http, keys).await?;
        if review.member_id != member || review.role != role {
            return Err(Error::Protocol(
                "Your signed-in account or role changed. Review the account again.",
            ));
        }
        let state = keys.link_state(&review);
        if !matches!(state, LinkState::Eligible | LinkState::Linked) {
            return Err(Error::Protocol(
                "This device cannot link that membership. Review the account again.",
            ));
        }
        let member_key = keys
            .membership
            .public_key()
            .as_ref()
            .try_into()
            .expect("Ed25519 public key is 32 bytes");
        let device_key = keys
            .device
            .public_key()
            .as_ref()
            .try_into()
            .expect("Ed25519 public key is 32 bytes");
        let message = transcript(member, review.session_id, &member_key, &device_key);
        let proof = LinkAccount {
            membership_key: member_key,
            device_key,
            membership_signature: keys.membership.sign(&message).as_ref().to_vec(),
            device_signature: keys.device.sign(&message).as_ref().to_vec(),
        };
        let url = http
            .base
            .join(PATH)
            .map_err(|_| Error::Protocol("invalid profile link URL"))?;
        let response = http
            .http
            .put(url)
            .bearer_auth(&http.token)
            .header("content-type", "application/json")
            .body(serde_json::to_vec(&proof).map_err(|_| Error::Protocol("invalid account proof"))?)
            .send()
            .await?;
        match response.status().as_u16() {
            204 => Ok(()),
            401 | 403 => Err(Error::Protocol(
                "Sign in to this server again before linking.",
            )),
            409 => Err(Error::Protocol(
                "This membership or device is already bound. Review the account again.",
            )),
            _ => Err(Error::Protocol(
                "Could not confirm the link. Retry to check the server.",
            )),
        }
    }

    async fn link_review(
        &self,
        http: &HttpSession,
        keys: &ProfileCredentials,
    ) -> Result<AccountReview, Error> {
        if self.address.id()? != keys.server {
            return Err(Error::Protocol("profile keys belong to a different server"));
        }
        let url = http
            .base
            .join(PATH)
            .map_err(|_| Error::Protocol("invalid profile link URL"))?;
        let response = http
            .http
            .get(url)
            .bearer_auth(&http.token)
            .header(
                "x-sideband-profile-device",
                URL_SAFE_NO_PAD.encode(keys.device.public_key().as_ref()),
            )
            .send()
            .await?;
        if matches!(response.status().as_u16(), 401 | 403) {
            return Err(Error::Protocol(
                "Sign in to this server again before linking.",
            ));
        }
        let review: AccountReview = serde_json::from_slice(&read_bounded(response, 8192).await?)
            .map_err(|_| Error::Protocol("invalid account review"))?;
        let own = self.state();
        if own
            .read()
            .own_user
            .as_ref()
            .is_none_or(|user| user.id.as_ref() != review.member_id.as_bytes())
        {
            return Err(Error::Protocol(
                "The server account changed. Reconnect before linking.",
            ));
        }
        Ok(review)
    }
}

impl ProfileCredentials {
    fn link_state(&self, review: &AccountReview) -> LinkState {
        match &review.membership_key {
            Some(key) if key != self.membership.public_key().as_ref() => LinkState::OtherProfile,
            Some(_) if !review.device_active => LinkState::UnauthorizedDevice,
            Some(_) if review.device_session => LinkState::Linked,
            _ => LinkState::Eligible,
        }
    }
}
