# Profile sign-in

A profile lets a person join several Sideband servers from one installation.
Each community owns its membership records, usernames, roles, bans and
message history. A profile name is a local label. The person chooses their
username when they first join a server.

## Keys and trust

The desktop creates two independent 32-byte random secrets and stores them
in macOS Keychain or Windows Credential Manager. The app's local profile
file contains the display name, a random credential-store lookup ID, confirmed
memberships, their trust settings and any pending public recovery grants.
The roots stay native. The backup screen displays the recovery words so the
person can save them separately.

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

## Device removal

An operator can list the account's enrolled devices with
`ss-server user devices --username NAME list` and revoke one with
`ss-server user devices --username NAME revoke --id ID`. Supply
`--data-dir PATH` before `list` or `revoke` when using a custom data
directory. Revocation invalidates that device’s sessions and retains a record
of its revoked credential. The membership and its moderation history survive.

## Link an existing server account

After signing in, open **Link existing accounts** in profile settings. The
desktop checks the signed-in server's `profileAccountLinking` feature and
shows the username, hostname and highest role before asking for confirmation.
A server without this feature keeps its usual per-device login.

The authenticated `GET /api/v1/me/profile-link` returns only the caller's
member ID, username, role, session ID and current profile binding. The native
client sends its server-specific device public key in the
`X-Sideband-Profile-Device` header (unpadded base64url) so the server can
report whether this device is enrolled. The session ID is a non-secret
identifier distinct from the bearer credential.

On confirmation the native client rechecks the reviewed member and role. It
signs the [link transcript](../../crates/ss-core/src/profile_link.rs) with
both the server-specific membership key and the device key. The server
validates its current human session, member and signatures, then atomically
binds the keys and the current session to the existing user. Linking preserves
the account's password login, messages, roles and bans. Each selected account
commits independently. A retry after a lost reply or failed catalog write can
complete the same binding. Existing bearer tokens and passwords remain local
to the installation.

## File-and-words recovery

A backup can be saved and opened offline. Version 1 contains the membership
root and a JSON body with the profile name, server addresses, local member IDs,
usernames, trust policies, and one recovery grant for each membership. The
enrolled device signs each grant over its server-specific membership key and
local member ID, using the recovery transcript defined in
[the protocol code](../../crates/ss-proto/src/profile.rs).

The desktop generates 128 random bits per file and displays them as 12 English
BIP39 words with a transcription checksum. This entropy is the AES-128-GCM
key. The file begins with the ASCII bytes `Sideband backup`, a zero byte and
version byte 1, followed by a random 12-byte nonce and ciphertext with a
16-byte authentication tag. The header is AEAD associated data. A final
SHA-256 digest over the preceding bytes helps identify damaged downloads
before asking the person to reenter the words. AES-GCM authenticates the
contents even if someone replaces that digest. The decoder caps files at 8 MiB.
Every export creates fresh words. An earlier copy still needs its own words.

A successful restore installs the membership root together with a newly
generated device root in the destination OS credential store. It publishes
the profile descriptor, server list, and trust policies atomically on a fresh
installation. The source device secret, passwords and session tokens are
excluded. The relay plays no part in opening this file.

On the first trusted connection to each community, the new device sends
`Hello.recover_profile` (tag 12) with a connection-bound device proof, a
membership signature over its fresh device key, the saved member ID, and
the grant from the source device. The server checks the membership still
maps to that member, enforces current bans, and checks its source-device
enrollment and revocation records. A revoked target key is always refused.
A valid recovery adds a device to the existing local account in the same
transaction as issuing its session. A server reset or a missing membership
cannot register a new account through this operation.

After successful enrollment the new device signs in independently of the
source. Revoking the source before enrollment prevents the old grant from
adding a device. If a restored computer makes another backup while the
server is offline, it preserves the outstanding grant. After enrollment,
new backups carry a grant from the replacement device.
