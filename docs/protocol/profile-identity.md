# Profile sign-in

A profile lets a person join several Sideband servers from one installation.
Each community owns its membership records, usernames, roles, bans and
message history. A profile name is a local label. The person chooses their
username when they first join a server.

## Keys and trust

The desktop creates two independent 32-byte random secrets and stores them
in macOS Keychain or Windows Credential Manager. The app's local profile
file contains the display name and a random credential-store lookup ID.
Only the name crosses the native bridge to the UI.

For each server, the native client derives two Ed25519 signing seeds using
HMAC-SHA-256. For the membership seed, the HMAC key is the membership root
and the input is `Sideband/v1/membership\0` followed by the canonical server
address (host and QUIC port). The device seed uses the device root,
`Sideband/v1/device\0` and the same address. The
two resulting public keys are specific to that server. Using another hostname
or QUIC port selects a different membership. The server sees those public
keys and the username the user chose. It receives neither root.

## Enrollment and sign-in

1. Read the server's advertised capabilities and complete certificate
   verification. A client may offer the profile flow for `profileSignIn`.
2. On the verified QUIC connection, export 32 bytes of TLS keying material
   with label `EXPORTER-Sideband-profile-v1` and an empty context. The device
   signs the domain-separated login transcript containing the exporter and
   both public keys. The membership key signs the enrollment transcript
   authorizing that device key. The [transcript definitions](../../crates/ss-proto/src/profile.rs)
   give the exact byte order and prefixes.
3. Send `Hello.profile` with the device proof. For first enrollment include
   `register` with the chosen username, invite code where required, and
   membership signature. The server applies its ordinary registration and
   invite rules. It commits the user, membership, first device and session
   together. A rejected attempt leaves the invitation available.
4. On return, the server looks up the same membership and active device.
   It uses the existing user and issues a session without spending another
   invite. A captured device proof cannot be reused on another TLS connection.
   The usual saved-session and ban checks apply.

## Device removal and future recovery

An operator can list the account's enrolled devices with
`ss-server user devices --username NAME list` and revoke one with
`ss-server user devices --username NAME revoke --id ID`. Supply
`--data-dir PATH` before `list` or `revoke` when using a custom data
directory. Revocation invalidates that device’s sessions and retains a record
of its revoked credential. The membership and its moderation history survive.

This release enrolls the first device. An additional device presenting a
valid membership signature still needs an explicit enrollment path. Future
pairing and recovery must authorize a fresh device key while preserving the
existing server member, bans and revoked-device records. The membership root
is the recoverable material. The device root belongs to its installation and
must be excluded from transfers and backups. Linking a password account also
requires proof of access to that existing account.
