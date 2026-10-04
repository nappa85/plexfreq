#!/usr/bin/env bash
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
cargo fmt --manifest-path "$root/Cargo.toml" --all --check
cargo clippy --manifest-path "$root/Cargo.toml" --locked --all-targets --all-features -- -D warnings
cargo test --manifest-path "$root/Cargo.toml" --locked --all-features
cmake -S "$root" -B "$root/build/desktop" -DCMAKE_BUILD_TYPE=Release
cmake --build "$root/build/desktop" --parallel
ctest --test-dir "$root/build/desktop" --output-on-failure
