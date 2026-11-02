# SocialSpeak desktop

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
