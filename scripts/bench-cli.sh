#!/usr/bin/env bash
# Time whole argus commands with hyperfine, start to finish, on a generated
# tree of real git repos (never your own directories). Complements the
# criterion benchmarks, which time functions in isolation.
#
#   scripts/bench-cli.sh                 # 200 repos, warm cache
#   scripts/bench-cli.sh -n 1000         # more repos
#   scripts/bench-cli.sh --compare main  # that revision against the working tree
#   scripts/bench-cli.sh --discovery     # discovery alone, on 2,000 repos among
#                                        # ~18,000 directories
#   scripts/bench-cli.sh --cold          # also cold cache (sudo: purges the disk cache)
#   scripts/bench-cli.sh --export out.json   # hyperfine's JSON, for comparing later
#   ARGUS_BIN=/path/to/argus scripts/bench-cli.sh   # time another build
#
# Trees are cached under target/bench.noindex/ (keyed by size and the
# function that builds them), so only the first run pays for building one;
# so are builds for --compare, by commit. The `.noindex` suffix keeps
# Spotlight out: indexing fresh trees slows every system call in the walk.
# Sustained runs on macOS can swing between two speeds (scheduler and power
# state): compare builds in one run (--compare).
# --discovery with --compare is fair only back to the commit that made the
# discover example use argus's thread count (before, it used every CPU).
set -euo pipefail

repos=200
cold=false
discovery=false
compare=
export_to=
while (($#)); do
    case $1 in
        -n) repos=$2; shift 2 ;;
        --cold) cold=true; shift ;;
        --discovery) discovery=true; shift ;;
        --compare) compare=$2; shift 2 ;;
        --export) export_to=$2; shift 2 ;;
        -h|--help) sed -n '2,/^[^#]/p' "$0" | sed -n 's/^# \{0,1\}//p'; exit 0 ;;
        *) echo "unknown option: $1 (try --help)" >&2; exit 2 ;;
    esac
done

command -v hyperfine >/dev/null || { echo "needs hyperfine: brew install hyperfine (or cargo install --locked hyperfine)" >&2; exit 1; }

here=$(cd "$(dirname "$0")/.." && pwd)
bench=$here/target/bench.noindex

# `--export out.json` as hyperfine arguments, with `-suffix` before `.json`
# when one run writes several files.
export_args() {
    [[ -z $export_to ]] && return
    printf '%s\n' --export-json "${export_to%.json}${1:+-$1}.json"
}

# The current build: the `argus` binary, or with `example`, the `discover`
# example's.
build_here() {
    if [[ ${1:-} == example ]]; then
        cargo build --release --quiet --example discover --manifest-path "$here/crates/argus-core/Cargo.toml"
        echo "$here/target/release/examples/discover"
    else
        cargo build --release --quiet --manifest-path "$here/crates/argus-cli/Cargo.toml"
        echo "$here/target/release/argus"
    fi
}

# The same for revision $compare, built from `git archive` in a target dir
# of its own (dependencies built once, then shared by every revision).
# Prints nothing when that revision can't build it (e.g. no such example).
build_rev() {
    local sha out src
    sha=$(git -C "$here" rev-parse --short "$compare^{commit}")
    out=$bench/builds/$sha
    if [[ ${1:-} == example ]]; then out=$out/discover; else out=$out/argus; fi
    if [[ ! -x $out ]]; then
        src=$bench/src-$sha
        rm -rf "$src"
        mkdir -p "$src" "$(dirname "$out")"
        git -C "$here" archive "$sha" | tar -x -C "$src"
        echo "building $compare ($sha) ..." >&2
        if [[ ${1:-} == example ]]; then
            CARGO_TARGET_DIR=$bench/compare-target nice cargo build --release --quiet \
                --example discover --manifest-path "$src/crates/argus-core/Cargo.toml" 2>/dev/null \
                && cp "$bench/compare-target/release/examples/discover" "$out"
        else
            CARGO_TARGET_DIR=$bench/compare-target nice cargo build --release --quiet \
                --manifest-path "$src/crates/argus-cli/Cargo.toml" \
                && cp "$bench/compare-target/release/argus" "$out"
        fi
        rm -rf "$src"
    fi
    [[ -x $out ]] && echo "$out"
    return 0
}

label_here="working tree"
label_rev() {
    local sha
    sha=$(git -C "$here" rev-parse --short "$compare^{commit}")
    if [[ $sha == "$compare"* ]]; then echo "$sha"; else echo "$compare ($sha)"; fi
}


key_of() { declare -f "$@" | git hash-object --stdin | cut -c1-8; }

# Build tree $1 with function $2 (given the directory and the size $3) once.
cached_tree() {
    local dir=$1 make=$2 n=$3
    if [[ ! -d $dir ]]; then
        echo "building $dir ..." >&2
        rm -rf "$dir.tmp"
        mkdir -p "$dir.tmp"
        # git isolated from the user's config, with fixed identities and
        # dates, so trees are the same on every machine. Only while
        # building: argus is timed with the user's own git config, as it
        # runs for real.
        (
            export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1
            export GIT_AUTHOR_NAME=bench GIT_AUTHOR_EMAIL=bench@example.com
            export GIT_COMMITTER_NAME=bench GIT_COMMITTER_EMAIL=bench@example.com
            export GIT_AUTHOR_DATE=2026-01-01T00:00:00Z GIT_COMMITTER_DATE=2026-01-01T00:00:00Z
            "$make" "$dir.tmp" "$n"
        )
        mv "$dir.tmp" "$dir"
    fi
}

# --discovery: discovery alone, on $1/dev, laid out like a big ~/dev: 20
# owners, each with 100 repos (just a `.git` directory: discovery reads no
# further, and doesn't enter repos) and 100 projects without git that it
# has to walk through (8 directories each), plus notes and a node_modules.
make_discovery_tree() {
    local d=$1/dev o r
    for o in $(seq 0 19); do
        mkdir -p "$d/org$o/notes/"{2024,2025}/{drafts,old} "$d/org$o/site/node_modules/"{pkg1,pkg2}/lib
        for r in $(seq 0 99); do
            mkdir -p "$d/org$o/repo$r/.git" "$d/org$o/project$r/"{src/{a,b},docs,build/out,tmp}
        done
    done
}

if $discovery; then
    tree=$bench/discovery-$(key_of make_discovery_tree)
    cached_tree "$tree" make_discovery_tree 0
    dirs=$(find "$tree/dev" -type d -not -path '*/.git*' | wc -l | tr -d ' ')
    here_bin=$(build_here example)
    cmds=(--command-name="discovery ($label_here)" "$here_bin -n 1 $tree/dev")
    if [[ -n $compare ]]; then
        rev_bin=$(build_rev example)
        if [[ -n $rev_bin ]]; then
            cmds=(--command-name="discovery ($(label_rev))" "$rev_bin -n 1 $tree/dev" "${cmds[@]}")
        else
            echo "$compare has no discover example: timing the working tree only" >&2
        fi
    fi
    echo "discovery: 2000 repos, $dirs directories" >&2
    # shellcheck disable=SC2046
    hyperfine -N --warmup 3 --output null $(export_args discovery) "${cmds[@]}"
    exit 0
fi

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
               echo behind | git -C "$r" commit-tree "HEAD^{tree}" -p HEAD -m behind)" ;;
        6) git -C "$r" switch -qc feature ;;
        7) echo stashed >>"$r/README"; git -C "$r" stash -q ;;
    esac
}
export -f make_repo

make_repos() {
    local d=$1/dev o
    # directories discovery has to walk past or skip
    for o in 0 1 2 3 4 5 6 7 8 9; do
        mkdir -p "$d/org$o/notes/drafts/old" "$d/org$o/site/node_modules/pkg/lib"
    done
    # shellcheck disable=SC2016 # $0 is expanded by the inner bash
    seq 0 $(($2 - 1)) | xargs -P 8 -I{} bash -c 'make_repo {} "$0"' "$d"
}

fixture=$bench/fixture-$repos-$(key_of make_repo make_repos)
cached_tree "$fixture" make_repos "$repos"
dev=$fixture/dev

printf "roots = ['%s']\n" "$dev" >"$fixture/config.toml"
export ARGUS_CONFIG=$fixture/config.toml ARGUS_STATE=$fixture/state.toml NO_COLOR=1

ARGUS_BIN=${ARGUS_BIN:-$(build_here)}
commands=("status" "status --all" "--json status --all")

if [[ -n $compare ]]; then
    rev_bin=$(build_rev)
    [[ -n $rev_bin ]] || { echo "couldn't build $compare" >&2; exit 1; }
    echo "$(label_rev) against the working tree; $repos repos" >&2
    # one hyperfine run per command, so each summary compares the two builds
    for i in "${!commands[@]}"; do
        c=${commands[$i]}
        # shellcheck disable=SC2046
        hyperfine -N --warmup 3 $(export_args "$i") \
            --command-name="$c ($(label_rev))" "$rev_bin $c" \
            --command-name="$c ($label_here)" "$ARGUS_BIN $c"
    done
else
    echo "argus: $ARGUS_BIN; $repos repos" >&2
    cmds=()
    for c in "${commands[@]}"; do cmds+=(--command-name="$c" "$ARGUS_BIN $c"); done
    # shellcheck disable=SC2046
    hyperfine -N --warmup 3 $(export_args) "${cmds[@]}"
fi

if $cold; then
    case $(uname) in
        Darwin) purge='sudo purge' ;;
        *) purge='sync; echo 3 | sudo tee /proc/sys/vm/drop_caches >/dev/null' ;;
    esac
    sudo -v   # ask for the password once, up front
    # shellcheck disable=SC2046
    hyperfine --runs 5 --prepare "$purge" $(export_args cold) \
        --command-name="status (cold cache)" "$ARGUS_BIN status"
fi
