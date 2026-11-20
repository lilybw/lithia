#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 lithia Authors
# SPDX-License-Identifier: GPL-2.0-or-later

# Runs every test binary of the workspace many times: in release, in debug, and in release pinned to
# one core (Linux only). Usage: stress.sh <release runs> <debug runs> <one-core runs> [out dir].
# Prints one line per binary and mode; keeps the output of every failed run; exits 1 if any failed.
set -uo pipefail

release_runs="${1:?release runs}"
debug_runs="${2:?debug runs}"
single_runs="${3:?one-core runs}"
out="${4:-target/stress}"
mkdir -p "$out"
command -v jq > /dev/null || { echo "stress.sh needs jq" >&2; exit 2; }

# The test executables cargo builds, except the compile-fail cases (they need cargo), loom, and the
# proc-macro crate's (linked to the toolchain's libstd, which only `cargo test` puts on the path).
binaries() {
    cargo test --workspace --locked --no-run "$@" || exit 2
    cargo test --workspace --locked --no-run --message-format=json "$@" 2> /dev/null |
        jq -r 'select(.profile.test == true and .executable != null and (.target.kind | index("proc-macro") | not))
            | "\(.target.name) \(.executable)"' |
        grep -vE '^(compile|loom) ' | sort -u
}

failed=0
stress() {
    local mode="$1" runs="$2" prefix="$3" list="$4"
    [[ -n "$list" ]] || { echo "no test binaries found" >&2; exit 2; }
    while read -r name binary; do
        local ok=0
        for ((i = 1; i <= runs; i++)); do
            if output="$($prefix "$binary" --quiet 2>&1 < /dev/null)"; then
                ok=$((ok + 1))
            else
                printf '%s\n' "$output" > "$out/fail-$mode-$name-$i.log"
                printf '%s\n' "$output" | grep -E -m 8 "^---- |panicked at|timed out|left:|right:" | sed "s/^/  run $i: /"
                failed=1
            fi
        done
        echo "$mode $name: $ok/$runs"
    done <<< "$list"
}

release="$(binaries --release)" || exit 2
debug="$(binaries)" || exit 2
stress release "$release_runs" "" "$release"
stress debug "$debug_runs" "" "$debug"
if command -v taskset > /dev/null && [[ "$single_runs" -gt 0 ]]; then
    stress one-core "$single_runs" "taskset -c 0" "$release"
fi
exit "$failed"
