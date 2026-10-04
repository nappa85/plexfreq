#!/usr/bin/env bash
set -euo pipefail
exec docker run --rm --entrypoint env "${SDK_IMAGE:-plexfreq-audio-sdk:local}" \
    PKG_CONFIG_SYSROOT_DIR="/srv/mer/targets/${SDK_TARGET:-SailfishOS-5.2.0.15-aarch64}" \
    PKG_CONFIG_LIBDIR="/srv/mer/targets/${SDK_TARGET:-SailfishOS-5.2.0.15-aarch64}/usr/lib64/pkgconfig:/srv/mer/targets/${SDK_TARGET:-SailfishOS-5.2.0.15-aarch64}/usr/lib/pkgconfig:/srv/mer/targets/${SDK_TARGET:-SailfishOS-5.2.0.15-aarch64}/usr/share/pkgconfig" \
    pkg-config "$@"
