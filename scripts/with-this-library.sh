#!/usr/bin/env bash
# Runs a cargo command in a program that uses lib-sync, with lib-sync taken from this checkout instead of the version the
# program's Cargo.lock holds, so a library change is built and tested where it is used (Droidtop/tracker#867 D2).
#
#   scripts/with-this-library.sh <the program's Cargo folder> <cargo subcommand and arguments...>
#
# Only the crates the program's Cargo.lock has are patched in, and `cargo update -p` moves them to this checkout
# first: a patch whose version differs from the locked one is otherwise dropped with a warning, and the program
# would build the old release instead (Droidtop/tracker#798). Any "was not used in the crate graph" fails the run.
# A program that does not use lib-sync at all is skipped with a notice.
set -euo pipefail
here="$(cd "$(dirname "$0")/.." && pwd)"
user="$1"; shift
url="https://github.com/Droidtop/lib-sync"
# Paths given relative to where this runs, resolved before moving into the program.
agent=""
[ -n "${AGENT_DIR:-}" ] && agent="$(cd "$AGENT_DIR" && pwd)"
cd "$user"
[ -f Cargo.lock ] || cargo generate-lockfile
if ! grep -q "$url" Cargo.lock; then
    echo "::notice::$(basename "$(pwd)") does not use lib-sync; nothing to build against it"
    exit 0
fi
args=(); names=()
for spec in sync-core:crates/core sync-store:crates/store sync-pair:crates/pair; do
    name="${spec%%:*}"; path="${spec#*:}"
    grep -q "^name = \"$name\"$" Cargo.lock || continue
    args+=(--config "patch.\"$url\".$name.path=\"$here/$path\"")
    # Every locked copy (a lock can hold two versions of one crate): name each, or `-p` is ambiguous.
    for version in $(awk -v n="$name" '$0 == "name = \"" n "\"" { getline; sub(/^version = "/, ""); sub(/"$/, ""); print }' Cargo.lock); do
        names+=(-p "$name@$version")
    done
done
# With AGENT_DIR (a droidtop-agent checkout of a ci/ branch that goes with this change), the program's droidtop-agent
# crates come from there too, so a change all three repositories make together is built together.
if [ -n "$agent" ]; then
    for dir in "$agent"/crates/*/; do
        name="$(sed -n 's/^name = "\(.*\)"$/\1/p' "$dir/Cargo.toml" | head -1)"
        grep -q "^name = \"$name\"$" Cargo.lock || continue
        args+=(--config "patch.\"https://github.com/Droidtop/droidtop-agent\".$name.path=\"$dir\"")
        for version in $(awk -v n="$name" '$0 == "name = \"" n "\"" { getline; sub(/^version = "/, ""); sub(/"$/, ""); print }' Cargo.lock); do
            names+=(-p "$name@$version")
        done
    done
fi
log="$(mktemp)"
cargo "${args[@]}" update "${names[@]}" 2>&1 | tee "$log"
cargo "${args[@]}" "$@" 2>&1 | tee -a "$log"
if grep -q "was not used in the crate graph" "$log"; then
    echo "::error::a patch of lib-sync was not used: the program did not build against this commit" >&2
    exit 1
fi
