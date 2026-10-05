#!/usr/bin/env bash
# Host Rust build scripts (ring) compile C against the actual Sailfish sysroot.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
cargo_home="${CARGO_HOME:-$HOME/.cargo}"
exec docker run --rm --user "$(id -u):$(id -g)" \
    -v "$root:$root" -v "$cargo_home:$cargo_home:ro" \
    -w "$PWD" --entrypoint /opt/cross/bin/aarch64-meego-linux-gnu-gcc \
    "${SDK_IMAGE:-plexfreq-audio-sdk:local}" \
    -B/opt/cross/bin/aarch64-meego-linux-gnu- \
    --sysroot="/srv/mer/targets/${SDK_TARGET:-SailfishOS-5.2.0.15-aarch64}" "$@"
