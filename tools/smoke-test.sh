#!/usr/bin/env bash
set -euo pipefail
state="$(mktemp -d "${TMPDIR:-/tmp}/plexfreq-smoke.XXXXXX")"
trap 'rm -rf "$state"' EXIT
PLEXFREQ_STATE_DIR="$state" "$1" --smoke-test
