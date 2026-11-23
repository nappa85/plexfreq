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
# dbus-run-session bus serving as both session and system bus. That bus has no
# service directories: with the standard ones it would auto-start the host's
# org.freedesktop.Notifications daemon (or any other activatable service) the
# first time a test posts a notification, on the developer's real display.
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

# The session.conf policy without <standard_session_servicedirs/> or any
# <servicedir>, so nothing on the host can be activated.
cat > "$runtime/bus.conf" <<'CONF'
<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>session</type>
  <keep_umask/>
  <listen>unix:tmpdir=/tmp</listen>
  <auth>EXTERNAL</auth>
  <policy context="default">
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
    <allow own="*"/>
  </policy>
</busconfig>
CONF

status=0
dbus-run-session --config-file="$runtime/bus.conf" -- bash -c \
    'export DBUS_SYSTEM_BUS_ADDRESS="$DBUS_SESSION_BUS_ADDRESS"; exec "$@"' \
    bash "$@" || status=$?
exit "$status"
