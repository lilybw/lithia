#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 lithia Authors
# SPDX-License-Identifier: MPL-2.0

# Runs TLC on one model: tlc.sh <model> [TLC options], from docs/tla/models. Fetches the TLA+ tools'
# current release into TLA_TOOLS (default: target/tla2tools.jar at the repository root) unless present.
set -euo pipefail

model="${1:?model}"
shift
root="$(git rev-parse --show-toplevel)"
jar="${TLA_TOOLS:-$root/target/tla2tools.jar}"
# The rolling v1.8.0 release: v1.7.4, the last fixed one, takes hours over Lifetimes.tla.
url="https://github.com/tlaplus/tlaplus/releases/download/v1.8.0/tla2tools.jar"
if [[ ! -f "$jar" ]]; then
    mkdir -p "$(dirname "$jar")"
    curl -sSLf -o "$jar.part" "$url"
    mv "$jar.part" "$jar"
fi
unzip -p "$jar" META-INF/MANIFEST.MF | grep -E "^Implementation-Version" || true
states="$(mktemp -d)"
trap 'rm -rf "$states"' EXIT
java -XX:+UseParallelGC -XX:MaxRAMPercentage=75 -DTLA-Library=.. -cp "$jar" tlc2.TLC \
    -workers auto -metadir "$states" "$@" "$model"
