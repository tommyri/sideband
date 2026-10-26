# T-073 — Screen share UI integration

Status: todo
Phase: 5
Size: S
Depends on: T-071, T-072, T-044
Paths: `apps/desktop/src/features/stream/`

## Goal
Start, watch, and stop streams from the voice UI.

## Scope
- "Share screen" button in the self panel (requires `STREAM`): source
  picker modal (displays/windows with thumbnails, preset, cursor toggle),
  live "You are streaming" pill with stop button and viewer count.
- Streaming indicator on members in the channel tree; "Watch" button on
  hover/context menu opens the viewer (T-072); watching state shown to the
  streamer (viewer count only, no names unless the group wants it — make it a
  server setting).
- Permission prompts surfaced (macOS Screen Recording, Wayland portal).

## Out of scope
Stream chat overlays.

## Acceptance criteria
- End-to-end manual test across OSes recorded in notes.
- Component tests for picker state and permission-gated visibility.

## Notes from implementation
