#!/usr/bin/env bash
# Names the next nightly release and builds its assets into target/dist.
# Writes `tag` (empty when HEAD is already released) and `previous` to $GITHUB_OUTPUT, and the notes to target/release-notes.md.
set -euo pipefail

base="$(cargo metadata --no-deps --format-version 1 --locked | jq -r '.packages[] | select(.name == "lithia") | .version')"
released="$(git tag --points-at HEAD --list "v${base}.*")"
if [[ -n "$released" ]]; then
    echo "HEAD is already released as ${released}; nothing to do."
    echo "tag=" >> "$GITHUB_OUTPUT"
    exit 0
fi

last="$(git tag --list "v${base}.*" | sed "s/^v${base}\.//" | grep -E '^[0-9]+$' | sort -n | tail -n 1 || true)"
tag="v${base}.$(( ${last:-0} + 1 ))"
previous="$(git describe --tags --abbrev=0 --match 'v*' HEAD 2>/dev/null || true)"

# Cargo verifies lithia against the packaged lithia-macro only when both are publishable.
sed -i 's/^publish = false$/publish = true/' Cargo.toml
cargo package -p lithia-macro -p lithia --locked --allow-dirty
git checkout -- Cargo.toml
cargo doc --no-deps -p lithia -p lithia-macro --locked

rm -rf target/dist && mkdir -p target/dist
cp "target/package/lithia-macro-${base}.crate" "target/package/lithia-${base}.crate" target/dist/
tar -czf "target/dist/lithia-docs-${tag}.tar.gz" -C target doc
spec="$(printf "%s\n" docs/spec/LITHIA_design_spec_v*.pdf | sort -V | tail -n 1)"
cp "$spec" target/dist/
(cd target/dist && sha256sum -- * > SHA256SUMS)

{
    echo "Automated nightly release of \`$(git rev-parse --short HEAD)\`, built after the test matrix passed on Ubuntu and Windows."
    echo
    echo "Assets: the \`lithia\` and \`lithia-macro\` crate packages (version ${base}), the API docs, the design spec and SHA-256 sums."
    echo
    if [[ -n "$previous" ]]; then
        echo "Changes since ${previous}:"
        echo
        git log --no-merges --format='- %s (%h)' "${previous}..HEAD"
    else
        echo "Recent changes:"
        echo
        git log --no-merges --format='- %s (%h)' -n 30 HEAD
    fi
} > target/release-notes.md

echo "tag=${tag}" >> "$GITHUB_OUTPUT"
echo "previous=${previous}" >> "$GITHUB_OUTPUT"
echo "Prepared ${tag}:"
ls -l target/dist
