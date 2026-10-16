# Headless PulseAudio for tools/pulse-test-server.sh on hosts without a
# `pulseaudio` binary (e.g. PipeWire desktops). Same distribution and daemon
# version as the CI runner (ubuntu-24.04, PulseAudio 16.1). No devices, no
# network: the daemon only serves null sinks on a bind-mounted unix socket.
FROM ubuntu:24.04
RUN apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends pulseaudio pulseaudio-utils \
    && rm -rf /var/lib/apt/lists/*
