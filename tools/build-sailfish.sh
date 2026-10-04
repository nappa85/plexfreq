#!/usr/bin/env bash
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
target="${SDK_TARGET:-SailfishOS-5.2.0.15-aarch64}"
image="${SDK_IMAGE:-plexfreq-audio-sdk:local}"
triple=aarch64-unknown-linux-gnu
installed="$(rustup target list --installed)"
if [[ "$installed" != *"$triple"* ]]; then
    printf 'Install the Rust target first: rustup target add %s\n' "$triple" >&2
    exit 1
fi
export CC_aarch64_unknown_linux_gnu="$root/tools/sdk-cc.sh"
export AR_aarch64_unknown_linux_gnu=ar
export SDK_TARGET="$target" SDK_IMAGE="$image"
bash "$root/tools/build-qm.sh" --check
if ! docker image inspect "$image" >/dev/null 2>&1; then docker build -f "$root/tools/audio-sdk.Dockerfile" -t "$image" "$root"; fi
export PKG_CONFIG_aarch64_unknown_linux_gnu="$root/tools/sdk-pkg-config.sh"
export PKG_CONFIG_ALLOW_CROSS=1
cargo build --manifest-path "$root/Cargo.toml" --locked --release --target "$triple" --lib
mkdir -p "$root/app/thirdparty" "$root/build/sailfish"
install -m 0644 "$root/target/$triple/release/libplexfreq_core.a" "$root/app/thirdparty/"
# A disposable copy prevents SDK ownership changes/build debris in source files.
cp -a "$root/app/." "$root/build/sailfish/"
owner="$(id -u):$(id -g)"
restore_owner() {
    docker run --rm --user root -v "$root/build/sailfish:/home/mersdk/app" \
        --entrypoint chown "$image" -R "$owner" /home/mersdk/app
}
trap restore_owner EXIT
docker run --rm --user root -v "$root/build/sailfish:/home/mersdk/app" \
    --entrypoint chown "$image" -R mersdk:mersdk /home/mersdk/app
docker run --rm -v "$root/build/sailfish:/home/mersdk/app" \
    -w /home/mersdk/app --entrypoint mb2 "$image" --target "$target" build
printf 'RPMs: %s/build/sailfish/RPMS/\n' "$root"
tar -C "$root/build/sailfish" -czf "$root/build/plexfreq-0.1.0-aarch64-rootless.tar.gz" harbour-plexfreq qml translations
printf 'Rootless bundle: %s/build/plexfreq-0.1.0-aarch64-rootless.tar.gz\n' "$root"
