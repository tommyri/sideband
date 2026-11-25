# Sideband desktop

The desktop client is a Tauri 2 shell around `ss-client-core`. React owns only
presentation and local UI state; connections, certificates, session storage,
HTTP, and protocol commands remain in Rust.

## Development

```sh
pnpm install
pnpm check
pnpm tauri dev
```

From the repository root, `just dev-desktop`, `just build-desktop`, and
`just check-desktop` provide the same workflows.

The checked-in bridge types live in `src/lib/bindings.ts`. A Rust test verifies
that they still match the exported Rust command and event types.

### Compact call browser test

From `apps/desktop`, install the headless browser once and run the test.

```sh
pnpm exec playwright install --only-shell chromium
pnpm run test:compact
```

The command builds the frontend and opens a fresh Chromium page at 760 × 600
for each scenario. A local bridge fixture supplies the chat, long room name
and active call. The tests exercise keyboard access to Audio, focus after
Escape, sending a two-line reply, and leaving voice with the reply intact. They also cover an interruption
during a reply, automatic voice return, manual rejoin and leaving while offline.
The host scenario copies a complete invitation from the compact server menu.
The frontend CI workflow runs the same checks.
