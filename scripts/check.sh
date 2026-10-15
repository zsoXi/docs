#!/usr/bin/env bash
# Local publication gate; dependencies must already be cached.
set -euo pipefail
export CARGO_NET_OFFLINE=true
export CARGO_BUILD_JOBS=2
cargo fmt --check
cargo test
cargo clippy --all-targets -- -D warnings
