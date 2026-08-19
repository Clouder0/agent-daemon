# agent-daemon tasks. Local commands mirror CI exactly.

default:
    @just --list

# Build all targets (bins + tests).
build:
    cargo build --all-targets

# Apply formatting.
fmt:
    cargo fmt

# Everything CI checks on a PR. Must pass before every PR.
lint:
    cargo fmt --check
    cargo clippy --all-targets -- -D warnings

# Unit tests (cargo-nextest).
test:
    cargo nextest run

# Advisories + license audit.
deny:
    cargo deny check advisories licenses

# Coverage, diagnostic only (never a gate).
coverage:
    cargo llvm-cov nextest --lcov --output-path lcov.info
