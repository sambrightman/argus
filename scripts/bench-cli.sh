#!/usr/bin/env bash
# Time whole argus commands with hyperfine, start to finish, on a generated
# tree of real git repos (never your own directories). Complements the
# criterion benchmarks, which time functions in isolation.
#
#   scripts/bench-cli.sh                 # 200 repos, warm cache
#   scripts/bench-cli.sh -n 1000         # more repos
#   scripts/bench-cli.sh --cold          # also cold cache (sudo: purges the disk cache)
#   scripts/bench-cli.sh --export out.json   # hyperfine's JSON, for comparing later
#   ARGUS_BIN=/path/to/argus scripts/bench-cli.sh   # time another build
#
# The tree is cached under target/bench/ (keyed by the repo count and this
# script's contents), so only the first run pays for building it.
# Sustained runs on macOS can swing between two speeds (scheduler and
# power state): compare builds in one hyperfine run, or repeat.
set -euo pipefail

repos=200
cold=false
export_args=()
while (($#)); do
    case $1 in
        -n) repos=$2; shift 2 ;;
        --cold) cold=true; shift ;;
        --export) export_args=(--export-json "$2"); shift 2 ;;
        -h|--help) sed -n '2,15p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "unknown option: $1 (try --help)" >&2; exit 2 ;;
    esac
done

command -v hyperfine >/dev/null || { echo "needs hyperfine: brew install hyperfine (or cargo install --locked hyperfine)" >&2; exit 1; }

here=$(cd "$(dirname "$0")/.." && pwd)
if [[ -z ${ARGUS_BIN:-} ]]; then
    cargo build --release --quiet -p argus --manifest-path "$here/Cargo.toml"
    ARGUS_BIN=$here/target/release/argus
fi

# git isolated from the user's config, with fixed identities and dates, so
# the tree is the same on every machine
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1
export GIT_AUTHOR_NAME=bench GIT_AUTHOR_EMAIL=bench@example.com
export GIT_COMMITTER_NAME=bench GIT_COMMITTER_EMAIL=bench@example.com
export GIT_AUTHOR_DATE=2026-01-01T00:00:00Z GIT_COMMITTER_DATE=2026-01-01T00:00:00Z

key=$(cat "$0" | git hash-object --stdin | cut -c1-8)
fixture=$here/target/bench/fixture-$repos-$key
dev=$fixture/dev

# One repo: a few commits in nested files, packed; then, by index, one of: clean,
# modified, staged, untracked, ahead of its upstream, behind it, on a
# feature branch, or with a stash. Upstreams are set without a real remote
# (a remote-tracking ref and branch config), so nothing touches the network.
make_repo() {
    local i=$1 dev=$2
    local r=$dev/org$((i % 10))/repo$i
    mkdir -p "$r/src/a/b"
    git -C "$r" init -q -b main
    for c in 1 2 3; do
        echo "line $c" >>"$r/README"
        printf 'fn f%s() {}\n' "$c" >>"$r/src/a/b/lib.rs"
        git -C "$r" add -A
        git -C "$r" commit -q -m "commit $c"
    done
    git -C "$r" remote add origin "https://github.com/org$((i % 10))/repo$i.git"
    git -C "$r" config branch.main.remote origin
    git -C "$r" config branch.main.merge refs/heads/main
    git -C "$r" update-ref refs/remotes/origin/main HEAD
    # packed objects and refs, as in a fresh clone; later commits stay loose
    git -C "$r" repack -adq
    git -C "$r" pack-refs --all
    git -C "$r" prune
    case $((i % 8)) in
        1) echo changed >>"$r/README" ;;
        2) echo staged >>"$r/README"; git -C "$r" add README ;;
        3) echo new >"$r/untracked" ;;
        4) echo ahead >>"$r/README"; git -C "$r" commit -qam ahead ;;
        5) git -C "$r" update-ref refs/remotes/origin/main "$(
               echo behind | git -C "$r" commit-tree HEAD^{tree} -p HEAD -m behind)" ;;
        6) git -C "$r" switch -qc feature ;;
        7) echo stashed >>"$r/README"; git -C "$r" stash -q ;;
    esac
}
export -f make_repo

if [[ ! -d $fixture ]]; then
    echo "building $repos repos in $fixture ..." >&2
    rm -rf "$fixture.tmp"
    mkdir -p "$fixture.tmp/dev"
    # directories discovery has to walk past or skip
    for o in 0 1 2 3 4 5 6 7 8 9; do
        mkdir -p "$fixture.tmp/dev/org$o/notes/drafts/old" \
                 "$fixture.tmp/dev/org$o/site/node_modules/pkg/lib"
    done
    seq 0 $((repos - 1)) | xargs -P 8 -I{} bash -c 'make_repo {} "$0"' "$fixture.tmp/dev"
    mv "$fixture.tmp" "$fixture"
fi

printf "roots = ['%s']\n" "$dev" >"$fixture/config.toml"
export ARGUS_CONFIG=$fixture/config.toml ARGUS_STATE=$fixture/state.toml NO_COLOR=1

cmds=(
    "$ARGUS_BIN status"
    "$ARGUS_BIN status --all"
    "$ARGUS_BIN --json status --all"
)
names=(--command-name "status" --command-name "status --all" --command-name "status --all --json")

echo "argus: $ARGUS_BIN; $repos repos" >&2
hyperfine -N --warmup 3 "${names[@]}" "${export_args[@]}" "${cmds[@]}"

if $cold; then
    case $(uname) in
        Darwin) purge='sudo purge' ;;
        *) purge='sync; echo 3 | sudo tee /proc/sys/vm/drop_caches >/dev/null' ;;
    esac
    sudo -v   # ask for the password once, up front
    hyperfine --runs 5 --prepare "$purge" --command-name "status (cold cache)" \
        "${export_args[@]/%.json/-cold.json}" "$ARGUS_BIN status"
fi
