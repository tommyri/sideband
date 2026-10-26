# Desktop client

Placeholder. The Tauri 2 application is scaffolded in task T-042.

It will live in this directory: `src-tauri/` for the Rust shell and `src/`
for the React UI. The shell is a thin bridge over `ss-client-core`. Audio,
the network connection, and push-to-talk stay in the Rust crates, not in the
webview.
