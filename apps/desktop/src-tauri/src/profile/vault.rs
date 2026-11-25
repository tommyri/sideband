use super::{ApiError, Vault, Zeroizing};

pub(super) struct System;

pub(super) fn available() -> Result<(), ApiError> {
    if cfg!(any(target_os = "macos", target_os = "windows")) {
        Ok(())
    } else {
        Err(ApiError::new(
            "profile_unavailable",
            "Local profiles are available on macOS and Windows.",
        ))
    }
}

#[cfg(target_os = "macos")]
impl Vault for System {
    fn put(&self, id: &str, secret: &[u8]) -> Result<(), ApiError> {
        security_framework::passwords::set_generic_password("Sideband/local-profile/v1", id, secret)
            .map_err(|_| super::locked())
    }
    fn read(&self, id: &str) -> Result<Zeroizing<Vec<u8>>, ApiError> {
        security_framework::passwords::get_generic_password("Sideband/local-profile/v1", id)
            .map(Zeroizing::new)
            .map_err(|_| super::locked())
    }
    fn remove(&self, id: &str) -> Result<(), ApiError> {
        security_framework::passwords::delete_generic_password("Sideband/local-profile/v1", id)
            .map_err(|_| super::locked())
    }
}

#[cfg(target_os = "windows")]
#[allow(
    unsafe_code,
    reason = "Windows Credential Manager's buffer allocation and UTF-16 APIs require FFI."
)]
mod windows_store {
    use super::{ApiError, System, Vault, Zeroizing};
    use windows::{
        Win32::Security::Credentials::{
            CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW, CredDeleteW, CredFree,
            CredReadW, CredWriteW,
        },
        core::{HSTRING, PWSTR},
    };

    fn target(id: &str) -> HSTRING {
        HSTRING::from(format!("Sideband/local-profile/v1/{id}"))
    }

    impl Vault for System {
        fn put(&self, id: &str, secret: &[u8]) -> Result<(), ApiError> {
            let name = target(id);
            let credential = CREDENTIALW {
                Type: CRED_TYPE_GENERIC,
                TargetName: PWSTR(name.as_ptr().cast_mut()),
                CredentialBlobSize: u32::try_from(secret.len())
                    .map_err(|_| super::super::locked())?,
                CredentialBlob: secret.as_ptr().cast_mut(),
                Persist: CRED_PERSIST_LOCAL_MACHINE,
                ..Default::default()
            };
            // SAFETY: CredWriteW only reads this structure and its buffers,
            // both of which remain live for the entire call.
            unsafe { CredWriteW(&raw const credential, 0) }.map_err(|_| super::super::locked())
        }

        fn read(&self, id: &str) -> Result<Zeroizing<Vec<u8>>, ApiError> {
            let mut ptr = std::ptr::null_mut();
            // SAFETY: on success Windows initializes ptr with one allocation
            // containing both the credential and its blob, freed once below.
            unsafe { CredReadW(&target(id), CRED_TYPE_GENERIC, None, &raw mut ptr) }
                .map_err(|_| super::super::locked())?;
            unsafe {
                let credential = &*ptr;
                let data = if credential.CredentialBlobSize == 64
                    && !credential.CredentialBlob.is_null()
                {
                    Ok(Zeroizing::new(
                        std::slice::from_raw_parts(credential.CredentialBlob, 64).to_vec(),
                    ))
                } else {
                    Err(super::super::locked())
                };
                CredFree(ptr.cast());
                data
            }
        }

        fn remove(&self, id: &str) -> Result<(), ApiError> {
            // SAFETY: the temporary owns the terminated UTF-16 name for this call.
            unsafe { CredDeleteW(&target(id), CRED_TYPE_GENERIC, None) }
                .map_err(|_| super::super::locked())
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
impl Vault for System {
    fn put(&self, _: &str, _: &[u8]) -> Result<(), ApiError> {
        available()
    }
    fn read(&self, _: &str) -> Result<Zeroizing<Vec<u8>>, ApiError> {
        Err(ApiError::new(
            "profile_unavailable",
            "System credential storage is unavailable.",
        ))
    }
    fn remove(&self, _: &str) -> Result<(), ApiError> {
        available()
    }
}
