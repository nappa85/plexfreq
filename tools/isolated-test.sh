#!/usr/bin/env bash
# Run a command isolated from the developer's audio server and D-Bus buses.
#
#   tools/isolated-test.sh COMMAND [ARGS...]
#
# PlexFreq's platform observers connect to $XDG_RUNTIME_DIR/pulse/native and to
# the session and system buses. On a desktop host they would see real
# microphone streams (the recording hold pauses playback) and Bluetooth state,
# and register MPRIS names on the user's session bus. COMMAND gets a fresh
# private runtime directory with no audio server, and one private
# dbus-run-session bus serving as both session and system bus.
# PLEXFREQ_TEST_ISOLATED=1 marks the environment for tests that assert it.
set -euo pipefail

runtime="$(mktemp -d)"
chmod 700 "$runtime"
cleanup() { rm -rf "$runtime"; }
trap cleanup EXIT
trap 'cleanup; exit 130' INT
trap 'cleanup; exit 143' TERM

export XDG_RUNTIME_DIR="$runtime"
export PULSE_SERVER="unix:$runtime/pulse/native"
export PLEXFREQ_TEST_ISOLATED=1

status=0
dbus-run-session -- bash -c \
    'export DBUS_SYSTEM_BUS_ADDRESS="$DBUS_SESSION_BUS_ADDRESS"; exec "$@"' \
    bash "$@" || status=$?
exit "$status"
