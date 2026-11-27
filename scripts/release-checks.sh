#!/bin/bash
# What a release commit must pass: CI's lint, docs and tests, the crates
# packaging as crates.io will take them, and notes for the new version in
# CHANGELOG.md. Run by `cargo release` before it commits (NEW_VERSION is
# the version being released); by hand, it checks Unreleased has notes.
# A pre-release (`1.2.0-beta.1`) leaves Unreleased as it is (cargo-release
# skips the changelog's replacements for one), so its notes are those.
set -euo pipefail
cd "$(dirname "$0")/.."
version=${NEW_VERSION:-Unreleased}
case $version in *-*) version=Unreleased ;; esac
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
# The dry run verifies each crate against the others packaged into a
# registry of its own, and cargo unpacks a crate's version from there
# once: a second dry run at the same version would build the TUI against
# the first run's core, missing what's been added since; and it takes a
# registry's crate as unchanging, so it would reuse the first run's build
# of it too. So unpack afresh (crates.io's own copies stay), and build
# in a folder of the dry run's own, cleared of argus's crates (the
# dependencies stay built there; other builds aren't touched).
for unpacked in "${CARGO_HOME:-$HOME/.cargo}"/registry/src/*/argus-git-*; do
    case $unpacked in */registry/src/index.crates.io-*) continue ;; esac
    if [ -d "$unpacked" ]; then rm -rf "$unpacked"; fi
done
export CARGO_BUILD_BUILD_DIR="$PWD/target/release-checks.noindex"
cargo clean -p argus-git-core -p argus-git-forge -p argus-git-tui -p argus-git
# (empty, an array is unset to bash 3.2, macOS's, under set -u)
cargo publish --workspace --locked --dry-run ${dirty[@]+"${dirty[@]}"}
