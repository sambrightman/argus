#!/bin/bash
# Delete object files in the build folder's debug/deps that no binary
# points at (the build folder: target/, or wherever `build.build-dir` puts
# intermediate files, e.g. target.noindex/).
#
# On macOS, debug builds leave each binary's debug info in its object files
# (split-debuginfo=unpacked), and cargo never deletes the old ones: every
# rebuild of argus leaves ~1,750 behind. Hundreds of thousands of them made
# builds and test startup stall. A binary's debug map (its OSO entries) names
# the object files it uses; anything else is garbage, so this costs no rebuild.
# Library code lives inside its .rlib, so only binaries need checking.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
build=$(cd "$root" && cargo metadata --format-version 1 --no-deps 2>/dev/null \
    | grep -o '"build_directory":"[^"]*"' | cut -d'"' -f4)
deps=${build:-${CARGO_TARGET_DIR:-$root/target}}/debug/deps
[ -d "$deps" ] || exit 0
cd "$deps"
used=$(mktemp)
trap 'rm -f "$used"' EXIT
for f in *; do
    [ -f "$f" ] && [ -x "$f" ] && [[ $f != *.* ]] || continue
    nm -ap "$f" 2>/dev/null | awk '$5 == "OSO" && $6 !~ /\(/ {n = split($6, p, "/"); print p[n]}'
done | sort -u > "$used"
n=0
while IFS= read -r o; do
    rm -f -- "$o"
    n=$((n + 1))
done < <(/bin/ls | grep '\.o$' | sort | comm -23 - "$used")
echo "pruned $n object files"
