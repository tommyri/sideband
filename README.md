# SocialSpeak

Self-hosted voice and text chat. TeamSpeak-class voice, Discord-class
features, one server binary, native clients for Windows, macOS, and Linux.

The implementation plan is in [`docs/plan`](docs/plan/00-overview.md). Tasks
live in [`docs/plan/tasks`](docs/plan/tasks/INDEX.md). Codec settings and the
decision record are in [`docs/design`](docs/design/README.md).

## Develop

Requires Rust 1.99.1 (pinned in `rust-toolchain.toml`), a nightly toolchain
with `rustfmt` (`rustup toolchain install nightly --profile minimal --component rustfmt`),
and [`just`](https://github.com/casey/just). Nightly is used only to format;
builds and tests use 1.99.1. Bundled libopus also requires CMake and a C/C++
compiler. Cargo defaults the macOS deployment target to 13.0.
Build prerequisites (macOS: Xcode Command Line Tools and `brew install cmake`; Linux:
`build-essential cmake`; Windows: Visual Studio C++ build tools and CMake).

```sh
just check
just run-server
just run-cli
```

`just check` is formatting, clippy with warnings denied, and the workspace
tests. CI runs the same command on Linux, macOS, and Windows.
