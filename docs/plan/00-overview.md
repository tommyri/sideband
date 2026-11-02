# SocialSpeak — Implementation Plan

Self-hosted voice + text chat for a friend group. TeamSpeak-class voice quality,
Discord-class features, one server binary, native desktop clients for Windows,
macOS and Linux.

This directory is the source of truth for *what* we build and *in which order*.
Sub-agents pick up individual tasks from `tasks/`. Read this file and
`04-conventions.md` before touching any task.

## Documents

| File | Purpose |
|---|---|
| `00-overview.md` | Goals, decisions, phases, how to work the plan (this file) |
| `01-architecture.md` | System architecture, crates, runtime model |
| `02-protocol.md` | Wire protocol: QUIC usage, control messages, media datagrams, REST |
| `03-data-model.md` | SQLite schema, permissions model, ids |
| `04-conventions.md` | Engineering conventions every task must follow |
| [`05-testing-app.md`](05-testing-app.md) | Current priority: installable testing app, evidence collection, release gates |
| [`../design/`](../design/README.md) | Why: codec contract, numbered decisions, constraints later tasks keep |
| `tasks/INDEX.md` | Task list with dependencies and status |
| `tasks/T-*.md` | One file per task, written to be picked up by a sub-agent |

## Goals (in priority order)

1. Voice quality and latency at least equal to TeamSpeak 3 (Opus, no forced
   processing, server never decodes audio).
2. Self-hostable on a small Linux VPS: one static binary + SQLite + a data dir.
3. Discord-style features: channel tree, voice + text channels, persistent
   history, replies, mentions, attachments, link previews, DMs, roles and
   permissions, kick/ban, notifications.
4. Native-feeling desktop clients for Windows, macOS, Linux from one codebase.
5. Screen sharing as a later phase that requires no protocol redesign.
6. Bot/webhook API later; mobile clients later.

## Key decisions

| # | Decision | Rationale |
|---|---|---|
| D1 | Rust everywhere (server, client core, shared crates). Tauri 2 for the desktop shell, React + TypeScript for UI. | Team preference; shared codec/protocol code; audio runs in Rust, never in a browser engine. |
| D2 | Our own transport on QUIC (`quinn`): one connection per client, control on a bidirectional stream, audio in datagrams, video (later) in one unidirectional stream per group of pictures. | Client→public server needs no ICE/TURN; QUIC gives TLS, multiplexing, one UDP port. Full control over Opus. |
| D3 | Do **not** build on the moq.dev transport/relay. Use `moq-video` (or `pinray` + encoders) purely as a capture/codec library in the screen-share phase. | Voice is small and must be exact; relay/pub-sub buys nothing for one server; avoid a fast-moving 0.x dependency at the core. |
| D4 | Server forwards opaque media payloads; it never decodes or knows codecs. | Zero generational loss, trivial CPU, DRED-friendly, video is a client-only addition. |
| D5 | Opus via `opusic-c` (libopus 1.6.x): 48 kHz. `voice` 48–96 kb/s (default 80) mono 20 ms VOIP, in-band FEC, 5% expected loss. `music` 96–192 kb/s (default 128) stereo 20 ms AUDIO, FEC on. `low_latency` 64 kb/s mono 10 ms. Deep PLC on; DRED behind a flag. Contract: [`docs/design/voice.md`](../design/voice.md). | Matches or exceeds TS3 Opus Voice q10; Discord caps at 64 kb/s mono with forced processing. |
| D6 | All audio processing (AEC, noise suppression, AGC) is opt-in and off by default. Pure-Rust `sonora` for APM; RNNoise/DeepFilterNet optional; `AUVoiceIO` as an alternative mode on macOS. | TeamSpeak philosophy; processing is what makes Discord sound worse. |
| D7 | Device I/O via `cpal` ≥ 0.18 with native PipeWire backend on Linux, WASAPI shared mode on Windows, CoreAudio on macOS. | Paths and reasons: [`docs/design/decisions.md`](../design/decisions.md) D7. Accurate timestamps for A/V sync. |
| D8 | Push-to-talk uses a raw global input listener per OS (hooks / listen-only CGEventTap / XInput2 / XDG GlobalShortcuts portal / opt-in evdev), **not** the Tauri global-shortcut plugin. | Hotkey registration APIs consume the key and can't bind mouse buttons; unacceptable for gaming PTT. |
| D9 | SQLite via `sqlx` with embedded migrations. | Friend-group scale; single-file backup; zero ops. |
| D10 | TLS: server generates a self-signed cert; clients pin on first use (TOFU, TeamSpeak/SSH style). Optional ACME when a domain is configured. | Works with a bare IP; no dependency on DNS. |
| D11 | Protobuf (`prost`) for control messages; the schema is the public API that bots will later use. | Typed, versionable, cross-language. |
| D12 | Roles/permissions, kick/ban, DMs, link previews and incoming webhooks are in the MVP. | Explicit requirement. |

## Phases

| Phase | Theme | Outcome |
|---|---|---|
| 0 | Foundations | Workspace, CI, protocol schema, core types, QUIC primitives |
| 1 | Voice spike | Headless CLI client + minimal forwarding server; A/B against TS3; parameters locked |
| 2 | Server core | Auth, roles, channels, messages, attachments, DMs, previews, moderation, webhooks, deploy |
| 3 | Desktop client | Tauri app: onboarding, channels, voice, chat, DMs, settings, PTT, admin UI, packaging |
| 4 | Hardening | Reconnect, security review, load test, diagnostics, docs |
| 5 | Screen share | Stream-based video tracks, capture/encode, viewer window, adaptive bitrate |
| 6 | Bot API | Bot accounts, REST + event stream, docs |
| 7 | Mobile | Not planned in detail; core crates already compile for iOS/Android |

The current priority is the installable testing-app milestone in
`05-testing-app.md`. T-018 is a voice-quality **release gate**, not a gate
on UI development, packaging, or explicitly labelled test builds. Its results
may revise voice defaults without invalidating the app shell.
Within phases 2 and 3 most tasks can proceed in parallel.

## How to work the plan (for humans and sub-agents)

1. Follow the priority order in `05-testing-app.md`, then the task index.
   Dependencies normally must be done. For this milestone, an implemented
   prerequisite with documented automated coverage may be integrated while
   its manual acceptance remains in progress. Record that distinction;
   never mark acceptance complete merely to unblock its test tooling.
2. Read the task file fully, then `04-conventions.md`, then the referenced
   sections of the architecture/protocol/data-model docs. If the task touches
   voice, transport, input, or a numbered decision, also read `docs/design/`.
3. Work on a branch named `task/T-xxx-short-name`. One task per branch/PR.
4. Set `Status:` in the task file to `in-progress` at start and `done` when
   merged. Record deviations in the task file under "Notes from implementation".
5. Definition of done: acceptance criteria met, tests added, `just check`
   passes on the OS(es) the task targets, docs updated if behavior changed.
6. If a task turns out to need a protocol or schema change, update
   `02-protocol.md` / `03-data-model.md` in the same PR and say so in the PR
   description. A change to a codec number or a numbered decision updates
   `docs/design/` in that same change.
7. Keep scope to the task. Note extra ideas in `tasks/BACKLOG.md`.

Task size legend: **S** ≈ one agent session, **M** ≈ 2–4 sessions, **L** ≈ a
week or more and should be split further before starting if possible.
