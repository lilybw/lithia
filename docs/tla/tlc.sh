#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 lithia Authors
# SPDX-License-Identifier: MPL-2.0

# Runs TLC on one model: tlc.sh <model> [TLC options], from docs/tla/models. Fetches the pinned TLA+
# tools into TLA_TOOLS (default: target/tla2tools.jar at the repository root) and checks their hash.
set -euo pipefail

model="${1:?model}"
shift
root="$(git rev-parse --show-toplevel)"
jar="${TLA_TOOLS:-$root/target/tla2tools.jar}"
url="https://github.com/tlaplus/tlaplus/releases/download/v1.7.4/tla2tools.jar"
sha256="936a262061c914694dfd669a543be24573c45d5aa0ff20a8b96b23d01e050e88"
if [[ ! -f "$jar" ]]; then
    mkdir -p "$(dirname "$jar")"
    curl -sSLf -o "$jar.part" "$url"
    mv "$jar.part" "$jar"
fi
echo "$sha256  $jar" | sha256sum -c --quiet -
states="$(mktemp -d)"
trap 'rm -rf "$states"' EXIT
java -XX:+UseParallelGC -XX:MaxRAMPercentage=75 -DTLA-Library=.. -cp "$jar" tlc2.TLC \
    -workers auto -metadir "$states" "$@" "$model"
