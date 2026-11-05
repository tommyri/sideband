# T-073 — Screen share UI integration

Status: in-progress
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

T-072's backend is implemented; its remaining hardware/latency acceptance does
not block this integration. Invoke `open_stream_viewer({ serverId, trackId })`
for a screen track, use `stream_viewers()` for diagnostics, and handle
`ss://viewer-error` (window label and message). The viewer owns subscribe,
unsubscribe, native presentation and window controls.


Implemented the desktop flow:
- Voice dock Share screen button gated by connection, current voice membership
  and effective STREAM permission. Source picker enumerates displays/windows,
  previews the selected source, offers 1080p60/1080p30/720p30 and a cursor toggle.
  No source is selected automatically. Wayland uses the system picker at start;
  macOS permission failures include recovery guidance and Refresh.
- Native capture runs on a dedicated local executor without retaining the client
  registry lock. Stop cancels startup/portal selection or publication; leaving,
  switching voice, disconnect/reconnect and permission loss release capture.
  Generation-bound commands prevent stale cleanup from stopping a new share.
- The dock shows starting/sharing/stopping, source and aggregate viewer count.
  Member rows show LIVE and Watch; watching requires the same voice channel,
  launches T-072's native window and reflects closing it. Viewer errors surface
  in the voice feedback area. Viewer identities are never disclosed.
- Linux desktop builds now enable the PipeWire capture backend; existing manual
  packaging already installs and bundles the required PipeWire dependency.
  GitHub Actions remain manually triggered.

Validation: `just check`, the desktop frontend checks (128 tests), production
frontend build, and native desktop tests passed. Six component tests cover source selection, options, permission loss,
permission-error retry, streaming/Stop and Watch gating/errors. Two native control
regressions verify correlated track IDs, non-replayable publication and cleanup
rejection across connection generations. A desktop regression verifies exact
connection membership, moderator moves and permission revocation. Chromium UI harness exercised Share →
selected-source preview → quality/cursor choices → streaming/viewer count → Watch
→ Stop at 1200×900 and 760×600, with no console errors or framework overlays.
The harness uses synthetic source pixels and bridge replies, not screen capture.

Remaining acceptance (task stays in-progress): install on Windows, macOS and
Linux; grant/deny recording or portal permissions; share a display and a window;
watch from another computer, close/reopen a viewer, cancel startup, stop sharing,
and leave/switch voice during capture. Record actual capture/encode/viewer behavior
and T-072 latency/lip-sync evidence. No hardware end-to-end results are claimed.
