# SocialSpeak task runner. `just check` is what CI runs.

check: fmt-check clippy test

# `imports_granularity` is still a rustfmt nightly option. Compilation stays
# on the stable toolchain in rust-toolchain.toml.
fmt:
    rustup run nightly cargo fmt --all

fmt-check:
    rustup run nightly cargo fmt --all -- --check

clippy:
    cargo clippy --workspace --all-targets -- -D warnings

test:
    cargo test --workspace

run-server:
    cargo run -p ss-server

run-cli:
    cargo run -p ss-cli

# Tauri runs the Vite dev server as its beforeDevCommand.
dev-desktop:
    cd apps/desktop && pnpm tauri dev

build-desktop:
    cd apps/desktop && pnpm tauri build

check-desktop:
    cd apps/desktop && CI=true pnpm check
    cargo test -p socialspeak-desktop
    cargo clippy -p socialspeak-desktop --all-targets -- -D warnings

# Fails if a crate depends on something above it in the graph.
# Unix only (bash). CI runs this on Linux.
check-deps:
    bash scripts/check-deps.sh
