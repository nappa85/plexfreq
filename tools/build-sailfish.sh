#!/usr/bin/env bash
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
target="${SDK_TARGET:-SailfishOS-5.2.0.15-aarch64}"
image="${SDK_IMAGE:-plexfreq-audio-sdk:local}"
triple=aarch64-unknown-linux-gnu
version="$(python3 "$root/tools/release-version.py" --print-version)"
# A plain assignment takes the substitution's exit status, so set -e already
# aborts when release-version.py reports a mismatch. Also reject unexpected
# output before it is used to name packages.
if [[ ! "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    printf 'Could not determine release version; refusing to package\n' >&2
    exit 1
fi
installed="$(rustup target list --installed)"
if [[ "$installed" != *"$triple"* ]]; then
    printf 'Install the Rust target first: rustup target add %s\n' "$triple" >&2
    exit 1
fi
# sdk-cc.sh mounts only the repository, so build output must stay inside it.
target_dir="$(realpath -m "${CARGO_TARGET_DIR:-$root/target}")"
if [[ "$target_dir" != "$root"/* ]]; then
    printf 'CARGO_TARGET_DIR must be inside %s\n' "$root" >&2
    exit 1
fi
export CC_aarch64_unknown_linux_gnu="$root/tools/sdk-cc.sh"
export AR_aarch64_unknown_linux_gnu=ar
export SDK_TARGET="$target" SDK_IMAGE="$image"
bash "$root/tools/build-qm.sh" --check
if ! docker image inspect "$image" >/dev/null 2>&1; then docker build -f "$root/tools/audio-sdk.Dockerfile" -t "$image" "$root"; fi
export PKG_CONFIG_aarch64_unknown_linux_gnu="$root/tools/sdk-pkg-config.sh"
export PKG_CONFIG_ALLOW_CROSS=1
CARGO_TARGET_DIR="$target_dir" cargo build --manifest-path "$root/Cargo.toml" --locked --release --target "$triple" --lib
# A disposable copy prevents SDK ownership changes/build debris in source files.
# Guard against an empty root (would delete /build/sailfish) and avoid dirtying
# the source tree: stage QML first, then install the archive into the stage.
rm -rf "${root:?}/build/sailfish"
mkdir -p "$root/build/sailfish/thirdparty"
cp -a "$root/app/." "$root/build/sailfish/"
install -m 0644 "$target_dir/$triple/release/libplexfreq_core.a" "$root/build/sailfish/thirdparty/"
owner="$(id -u):$(id -g)"
restore_owner() {
    local result=$?
    docker run --rm --user root -v "$root/build/sailfish:/home/mersdk/app" \
        --entrypoint chown "$image" -R "$owner" /home/mersdk/app || { local cleanup=$?; (( result != 0 )) || result=$cleanup; }
    exit "$result"
}
trap restore_owner EXIT
docker run --rm --user root -v "$root/build/sailfish:/home/mersdk/app" \
    --entrypoint chown "$image" -R mersdk:mersdk /home/mersdk/app
docker run --rm -v "$root/build/sailfish:/home/mersdk/app" \
    -w /home/mersdk/app --entrypoint mb2 "$image" --target "$target" build
printf 'RPMs: %s/build/sailfish/RPMS/\n' "$root"
