#!/usr/bin/env bash
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
image="${SDK_IMAGE:-plexfreq-audio-sdk:local}"
target="${SDK_TARGET:-SailfishOS-5.2.0.15-aarch64}"
stage="$root/build/device-tests"
library="$root/target/aarch64-unknown-linux-gnu/release/libplexfreq_core.a"
if [[ ! -f "$library" ]]; then
    printf 'Run tools/build-sailfish.sh first to build the Rust static library.\n' >&2
    exit 1
fi
# Start from an empty stage so files dropped from the resource cannot linger.
rm -rf "${stage:?}"
mkdir -p "$stage/thirdparty" "$stage/rpm" "$stage/tests" "$stage/app"
cp "$root/tests/qt-backend-test.qrc" "$stage/tests/"
# Stage exactly the QML the test resource embeds, at the same relative paths.
mapfile -t resources < <(sed -n 's|.*<file[^>]*>\([^<]*\)</file>.*|\1|p' "$root/tests/qt-backend-test.qrc")
if (( ${#resources[@]} == 0 )); then
    printf 'No files listed in tests/qt-backend-test.qrc\n' >&2
    exit 1
fi
for resource in "${resources[@]}"; do
    relative="$(realpath -m --relative-to="$root" "$root/tests/$resource")"
    if [[ "$relative" == ../* || ! -f "$root/$relative" ]]; then
        printf 'Invalid test resource: %s\n' "$resource" >&2
        exit 1
    fi
    install -D -m 0644 "$root/$relative" "$stage/$relative"
done
cp "$root/tests/qt_backend.cpp" "$stage/"
cp "$root/app/src/backend.cpp" "$root/app/src/backend.h" "$root/app/src/plexfreq_core.h" "$stage/"
cp "$root/app/src/i18n.cpp" "$root/app/src/i18n.h" "$stage/"
cp "$root/app/translations.qrc" "$stage/app/"
cp -a "$root/app/translations" "$stage/app/"
cp "$root/tests/sailfish/plexfreq-backend-test.pro" "$stage/"
cp "$root/tests/sailfish/rpm/plexfreq-backend-test.spec" "$stage/rpm/"
cp "$library" "$stage/thirdparty/"
owner="$(id -u):$(id -g)"
restore_owner() {
    local result=$?
    docker run --rm --user root -v "$stage:/home/mersdk/app" \
        --entrypoint chown "$image" -R "$owner" /home/mersdk/app || { local cleanup=$?; (( result != 0 )) || result=$cleanup; }
    exit "$result"
}
trap restore_owner EXIT
docker run --rm --user root -v "$stage:/home/mersdk/app" \
    --entrypoint chown "$image" -R mersdk:mersdk /home/mersdk/app
docker run --rm -v "$stage:/home/mersdk/app" -w /home/mersdk/app \
    --entrypoint mb2 "$image" --target "$target" build
printf 'Device fixture: %s/plexfreq-backend-test\n' "$stage"
