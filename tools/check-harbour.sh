#!/usr/bin/env bash
# Run the Jolla Harbour validation suite (mb2 check --suites harbour) on the
# RPM produced by tools/build-sailfish.sh in build/sailfish.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
target="${SDK_TARGET:-SailfishOS-5.2.0.15-aarch64}"
image="${SDK_IMAGE:-plexfreq-audio-sdk:local}"
stage="$root/build/sailfish"
if ! compgen -G "$stage/RPMS/*.rpm" >/dev/null; then
    printf 'No RPM in %s/RPMS; run tools/build-sailfish.sh first\n' "$stage" >&2
    exit 1
fi
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
docker run --rm -v "$stage:/home/mersdk/app" \
    -w /home/mersdk/app --entrypoint mb2 "$image" --target "$target" check --suites harbour
