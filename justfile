# Sideband task runner. `just check` is what CI runs.

check: fmt-check clippy test plan-check

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
    cargo run -p ss-server -- spike

run-cli:
    cargo run -p ss-cli

# Tauri runs the Vite dev server as its beforeDevCommand.
dev-desktop:
    cd apps/desktop && pnpm tauri dev

build-desktop:
    cd apps/desktop && pnpm tauri build

# Labelled packages plus build identity/checksums; pass --debug for local checks.
build-test-desktop *args:
    node scripts/build-test-desktop.mjs {{args}}

# Stops first, with the reason, when Node or pnpm is not the pinned version.
check-desktop:
    node scripts/check-desktop-toolchain.mjs
    cd apps/desktop && CI=true pnpm check
    cargo test -p socialspeak-desktop
    cargo clippy -p socialspeak-desktop --all-targets -- -D warnings

# Fails if a crate depends on something above it in the graph.
# Unix only (bash). CI runs this on Linux.
check-deps:
    bash scripts/check-deps.test.sh
    bash scripts/check-deps.sh

# Rewrite the dependency graph and waves in docs/plan/tasks/INDEX.md from its
# task tables. Rerun after changing a row's "Depends on" or adding a task.
plan-index:
    python3 scripts/plan-index.py

# Fails when the INDEX graph or waves are stale or a row disagrees with its task file.
plan-index-check:
    python3 scripts/plan-index.py --check

# Fails when 02-protocol.md and the axum routes or envelope oneofs disagree.
protocol-doc-check:
    python3 scripts/check-protocol-doc.py

# Fails when a relative link or #anchor in the repository's Markdown is broken.
# External URLs are not checked.
doc-links:
    python3 scripts/check-doc-links.py

# Rewrite docs/protocol/messages.md from the .proto comments.
proto-doc:
    python3 scripts/proto-doc.py

# Fails when docs/protocol/messages.md is stale.
proto-doc-check:
    python3 scripts/proto-doc.py --check

# Script tests plus the plan and documentation checks. Part of `just check`.
plan-check:
    python3 scripts/plan-index.test.py
    python3 scripts/check-protocol-doc.test.py
    python3 scripts/check-doc-links.test.py
    python3 scripts/proto-doc.test.py
    python3 scripts/plan-index.py --check
    python3 scripts/check-protocol-doc.py
    python3 scripts/proto-doc.py --check
    python3 scripts/check-doc-links.py
