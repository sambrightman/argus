#!/bin/bash
# What a release commit must pass: CI's lint, docs and tests, the crates
# packaging as crates.io will take them, and notes for the new version in
# CHANGELOG.md. Run by `cargo release` before it commits (NEW_VERSION is
# the version being released); by hand, it checks Unreleased has notes.
set -euo pipefail
cd "$(dirname "$0")/.."
version=${NEW_VERSION:-Unreleased}
notes=$(parse-changelog CHANGELOG.md "$version" | grep -v '^\*\*Full Changelog\*\*' | grep -c . || true)
if [ "$notes" -eq 0 ]; then
    echo "CHANGELOG.md has no notes for $version" >&2
    exit 1
fi
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --locked
cargo nextest run --workspace --locked
# (cargo release runs this before committing the new version: dirty then)
dirty=()
if [ -n "${NEW_VERSION:-}" ]; then dirty=(--allow-dirty); fi
cargo publish --workspace --locked --dry-run "${dirty[@]}"
