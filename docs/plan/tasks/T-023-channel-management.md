# T-023 — Channel tree management

Status: todo
Phase: 2
Size: M
Depends on: T-021, T-022
Paths: `crates/ss-server/src/handlers/channels.rs`, `crates/ss-server/src/snapshot.rs`

## Goal
Categories, text channels and voice channels with ordering, settings, and
the snapshot sent in `Welcome`.

## Scope
- Handlers: `CreateChannel`, `UpdateChannel`, `DeleteChannel`, `MoveChannel`
  under `MANAGE_CHANNELS`; validation (name 1–64, categories can't nest,
  voice settings only on voice channels, `user_limit` 0–99, bitrate within the
  profile's range from `VoiceProfile::encoder_params`).
- Deleting a channel soft-deletes it and its messages remain in DB
  (hidden); voice members are moved out (`VoiceStateUpdated` with no channel).
- Position handling: dense integers per parent; `MoveChannel` reorders
  siblings transactionally.
- Snapshot builder: channel tree filtered by `VIEW_CHANNEL`, roles, members
  with presence (from T-024), voice states and tracks (from T-025), read
  states, DM channels of the user.
- Events filtered per recipient by visibility; a channel becoming visible via
  a role change sends `ChannelCreated` to that user (handled in T-022 hook).

## Out of scope
DMs (T-028), messages.

## Acceptance criteria
- Integration tests for CRUD, ordering, validation errors, permission
  denials, and that a user without `VIEW_CHANNEL` never receives events for
  that channel.
- Snapshot for a fresh server contains the seeded defaults (one category,
  `general` text, `General` voice created by migration or first start).

## Notes from implementation
