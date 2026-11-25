# Device pairing v1

A trusted computer creates a five-minute invitation on the profile’s chosen sync server. The invitation grants access to the profile and its recorded server memberships. The destination generates a fresh local device root. Each community verifies its enrollment grant and issues its own current session when that computer signs in. Device preferences stay on each installation.

## Finding the relay

A pairing link uses `sideband://pair?relay=<address>#<code>`. The address contains the QUIC host and port, with an optional `?https=<port>` for a separate HTTPS listener. The link URL-encodes the address as one query parameter. Manual entry accepts the same address and code. Both clients normalize the host through `ServerAddr` and use the verified HTTPS policy. For a private certificate the invitation lookup binds the observed fingerprint. A change to a previously saved certificate requires separate verification.

## Exchange

The [pairing wire types](../../crates/ss-core/src/pairing.rs) define requests and replies. Requests POST a JSON object with `slot` and `action` to the owner or device route. A 32-byte value is a JSON byte array. The relay responds with a non-cacheable JSON reply. Each side creates an ephemeral X25519 key. The invitation contains a policy byte (0 for public certificates, 1 for a pinned certificate) followed by 16 random secret bytes, encoded as unpadded RFC 4648 Base32 and grouped for transcription.

1. The trusted computer authenticates to the owner route using its member bearer and scoped membership key. It sends a derived slot, ephemeral public key and offer MAC. A new code replaces the member’s previous slot.
2. The requester sends its public key, display name, the SHA-256 hash of a random 32-byte retrieval token, and a claim MAC. The slot accepts one immutable claim. Repeating the same claim returns the same offer.
3. Both computers authenticate the offer and claim and derive a shared channel. They independently display three English BIP39 words. The owner checks the name, membership count and phrase and explicitly approves or denies. A failed authentication ends the flow.
4. Approval uploads the encrypted portable profile and source-signed enrollment grants for the reviewed claim. The destination polls with its retrieval token. It checks approval again on **Finish linking**, decrypts locally and atomically installs the profile in its OS credential store. Acknowledgement removes the delivery. Retrying a fetch after an interrupted download is safe until acknowledgement or expiry.

For interoperable transcripts, start with ASCII `Sideband/v1/device-pairing`, a zero byte, the ASCII label and another zero byte. Prefix each subsequent field with its eight-byte unsigned big-endian length. HMAC-SHA256 with the invitation secret authenticates these transcripts:

| Label | Fields in order |
| --- | --- |
| slot | UTF-8 normalized server ID, HTTPS port and certificate policy joined with vertical bars. The policy is `public` or the lowercase hex fingerprint. |
| offer | Slot and trusted device public key |
| claim | Slot, requesting public key, token hash and UTF-8 device name |

Use the invitation secret as HKDF-SHA256 salt and the X25519 result as input key material. For the expand info, append the literal `encryption` or `phrase` to the `channel` transcript of slot, offer MAC and claim MAC. Each output is 32 bytes. Read the first eight phrase bytes as an unsigned big-endian integer. Its least-significant three groups of eleven bits index the word list, starting with the lowest group.

The encryption key seals the 32-byte membership root followed by the portable profile and grants JSON using ChaCha20-Poly1305 with the channel transcript as associated data. Each delivery uses a fresh random 96-bit nonce. The wire string is the unpadded base64url encoding of nonce, ciphertext and tag. The webview only receives invitation and review details. The code, shared key and plaintext profile stay off the relay.

## Lifetime and access

The relay keeps at most 64 invitations in memory, one per originating member, for five minutes. The maximum encoded delivery is 1 MiB. Every transition rechecks the originating session, enrolled device, membership and hosting setting. Approval binds the exact reviewed claim. Every successful delivery fetch needs the requesting device’s retrieval token. Restart, cancellation, denial, replacement, acknowledgement and expiry end access. A relay outage can be retried during the invitation lifetime. After expiry, create a new invitation and compare the new phrases.
