#!/usr/bin/env bash
# Run a command against a private, headless PulseAudio daemon.
#
#   tools/pulse-test-server.sh [--] COMMAND [ARGS...]
#
# The daemon has no hardware: two null sinks (plexfreq_a, the default, and
# plexfreq_b), each kept running by a silent pacat client, on a unix socket
# inside a fresh temporary directory. COMMAND sees
# PULSE_SERVER pointing at that socket and PLEXFREQ_PULSE_ISOLATED=1, which is the
# only signal that lets the server-backed tests move streams and load/unload
# sinks. The daemon and directory are removed when COMMAND exits (or on
# INT/TERM); COMMAND's exit status is returned.
#
# The developer's own audio server is never contacted: the daemon does not
# autospawn, read the user's runtime/config/state directories or use D-Bus, and
# clients are pinned to the private socket with autospawn disabled.
#
# PLEXFREQ_PULSE_BACKEND selects how the daemon runs:
#   native  `pulseaudio` from PATH (CI: apt pulseaudio on ubuntu-24.04)
#   docker  the same daemon in a container built from tools/pulse-test.Dockerfile
#           (hosts with PipeWire and no pulseaudio binary); --network none, no
#           devices, runs as the calling uid, socket shared via a bind mount
#   auto    native when available, otherwise docker (default)
# PLEXFREQ_PULSE_SHM=0 disables shared memory on both ends, so all audio and
# control data use the socket (no memfd/srbchannel); the default is shm, as a
# local desktop or phone client normally negotiates.
# `pactl`, `pacat`/`parec` and libpulse are needed on the host either way.
set -euo pipefail

root_dir="$(cd "$(dirname "$0")/.." && pwd)"
[[ "${1:-}" == "--" ]] && shift
if [[ $# -eq 0 ]]; then
    sed -n '2,/^set -euo/{/^#/s/^# \{0,1\}//p}' "$0" >&2
    exit 2
fi
for tool in pactl pacat; do
    command -v "$tool" >/dev/null || { echo "pulse-test-server: $tool is required" >&2; exit 2; }
done

backend="${PLEXFREQ_PULSE_BACKEND:-auto}"
if [[ "$backend" == auto ]]; then
    if command -v pulseaudio >/dev/null; then backend=native
    elif command -v docker >/dev/null; then backend=docker
    else echo "pulse-test-server: neither pulseaudio nor docker is available" >&2; exit 2
    fi
fi

# Unix socket paths are limited to 108 bytes: keep the directory short.
work="$(mktemp -d "${TMPDIR:-/tmp}/plexfreq-pulse.XXXXXX")"
mkdir -p "$work/run" "$work/home" "$work/config" "$work/state"
chmod 700 "$work/run"
socket="$work/run/native"
container=""
daemon=""
keepalive=()
child=""

# shellcheck disable=SC2329 # invoked by the EXIT trap
cleanup() {
    local status=$?
    trap - EXIT INT TERM
    if [[ -n "$child" ]] && kill -0 "$child" 2>/dev/null; then
        kill -TERM "$child" 2>/dev/null || true
        wait "$child" 2>/dev/null || true
    fi
    if [[ ${#keepalive[@]} -gt 0 ]]; then
        kill "${keepalive[@]}" 2>/dev/null || true
        wait "${keepalive[@]}" 2>/dev/null || true
    fi
    if [[ -n "$container" ]]; then
        docker rm -f "$container" >/dev/null 2>&1 || true
    fi
    if [[ -n "$daemon" ]] && kill -0 "$daemon" 2>/dev/null; then
        kill "$daemon" 2>/dev/null || true
        for _ in $(seq 50); do kill -0 "$daemon" 2>/dev/null || break; sleep 0.1; done
        kill -9 "$daemon" 2>/dev/null || true
        wait "$daemon" 2>/dev/null || true
    fi
    rm -rf "$work"
    exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

sink_args=(
    -L "module-null-sink sink_name=plexfreq_a rate=48000 channels=2 format=float32le sink_properties=device.description=PlexFreq-A"
    -L "module-null-sink sink_name=plexfreq_b rate=48000 channels=2 format=float32le sink_properties=device.description=PlexFreq-B"
)
daemon_args=(
    -n --daemonize=no --system=no --use-pid-file=no --exit-idle-time=-1
    --realtime=no --high-priority=no --log-level=info
)
shm="${PLEXFREQ_PULSE_SHM:-1}"
[[ "$shm" == 0 ]] && daemon_args+=(--disable-shm=yes)

case "$backend" in
native)
    command -v pulseaudio >/dev/null || { echo "pulse-test-server: pulseaudio not found" >&2; exit 2; }
    # A scrubbed environment: no session bus, no user runtime/config/state dirs.
    env -i PATH="$PATH" HOME="$work/home" XDG_RUNTIME_DIR="$work/run" \
        XDG_CONFIG_HOME="$work/config" XDG_STATE_HOME="$work/state" \
        PULSE_RUNTIME_PATH="$work/run" PULSE_STATE_PATH="$work/state" \
        pulseaudio "${daemon_args[@]}" --log-target="file:$work/pulse.log" \
        -L "module-native-protocol-unix socket=$socket auth-anonymous=1" \
        "${sink_args[@]}" &
    daemon=$!
    ;;
docker)
    image="plexfreq-pulse-test:local"
    if ! docker image inspect "$image" >/dev/null 2>&1; then
        docker build -q -f "$root_dir/tools/pulse-test.Dockerfile" -t "$image" "$root_dir/tools" >&2
    fi
    container="plexfreq-pulse-$$-$RANDOM"
    docker run -d --rm --name "$container" --network none --user "$(id -u):$(id -g)" \
        -e HOME=/tmp -e XDG_RUNTIME_DIR=/run/plexfreq-pulse \
        -v "$work/run:/run/plexfreq-pulse" "$image" \
        pulseaudio "${daemon_args[@]}" --log-target=stderr \
        -L "module-native-protocol-unix socket=/run/plexfreq-pulse/native auth-anonymous=1" \
        "${sink_args[@]}" >/dev/null
    ;;
*)
    echo "pulse-test-server: unknown PLEXFREQ_PULSE_BACKEND=$backend" >&2
    exit 2
    ;;
esac

printf 'autospawn = no\n' >"$work/client.conf"
[[ "$shm" == 0 ]] && printf 'enable-shm = no\n' >>"$work/client.conf"
export PULSE_SERVER="unix:$socket"
# The tests pin this copy: other unit tests repoint PULSE_SERVER in-process.
export PLEXFREQ_PULSE_TEST_SERVER="$PULSE_SERVER"
export PULSE_CLIENTCONFIG="$work/client.conf"
export PULSE_COOKIE="$work/cookie"
export PLEXFREQ_PULSE_ISOLATED=1
unset PULSE_SINK PULSE_SOURCE

ready=""
for _ in $(seq 100); do
    if [[ -S "$socket" ]] && pactl info >/dev/null 2>&1 \
        && pactl list short sinks 2>/dev/null | grep -q plexfreq_b; then
        ready=1
        break
    fi
    if [[ -n "$daemon" ]] && ! kill -0 "$daemon" 2>/dev/null; then break; fi
    sleep 0.1
done
if [[ -z "$ready" ]]; then
    echo "pulse-test-server: daemon did not become ready ($backend)" >&2
    [[ -f "$work/pulse.log" ]] && tail -n 40 "$work/pulse.log" >&2
    [[ -n "$container" ]] && docker logs "$container" 2>&1 | tail -n 40 >&2
    exit 1
fi
# Keep both sinks running like an active hardware sink. An idle null sink
# sleeps for up to its maximum latency (seconds), which delayed the first audio
# after an uncork or a move by 0.5-1.5 s and made its monitor drop data; that
# is a null-sink artefact, not something real devices or PlexFreq do. A silent
# low-latency client per sink avoids it; tests find their own stream by
# application.id.
for sink in plexfreq_a plexfreq_b; do
    pacat --playback --device="$sink" --latency-msec=40 --format=s16le --rate=48000 \
        --channels=2 --raw --client-name="plexfreq-keepalive-$sink" /dev/zero &
    keepalive+=($!)
done
for _ in $(seq 50); do
    [[ $(pactl list short sink-inputs | wc -l) -ge 2 ]] && break
    sleep 0.1
done
echo "pulse-test-server: $backend daemon (shm=$shm) ready at $PULSE_SERVER" \
    "($(LC_ALL=C pactl info | sed -n "s/^Server Version: //p"))" >&2

# In the background so INT/TERM run cleanup at once (the trap stops COMMAND).
set +e
"$@" &
child=$!
wait "$child"
status=$?
child=""
set -e
if [[ $status -ne 0 ]]; then
    echo "pulse-test-server: command failed ($status); daemon log tail:" >&2
    [[ -f "$work/pulse.log" ]] && tail -n 60 "$work/pulse.log" >&2
    [[ -n "$container" ]] && docker logs "$container" 2>&1 | tail -n 60 >&2
fi
exit "$status"
