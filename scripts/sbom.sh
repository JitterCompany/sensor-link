#!/usr/bin/env bash
# Write a CycloneDX SBOM (JSON, spec 1.5) for one workspace crate, listing the
# full transitive dependency graph. Extra arguments go to cargo-cyclonedx,
# e.g. --target or --all-features.
#
# Usage: ./scripts/sbom.sh <crate> <out-file> [cargo-cyclonedx args...]

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

CRATE="$1"
mkdir -p "$(dirname "$2")"
OUT="$(cd "$(dirname "$2")" && pwd)/$(basename "$2")"
shift 2

cd "$REPO_ROOT"

for cmd in cargo cargo-cyclonedx jq; do
    command -v "$cmd" >/dev/null || { echo "Required command not found: $cmd" >&2; exit 1; }
done

MEMBER_DIRS=$(cargo metadata --no-deps --format-version 1 \
    | jq -r '.packages[] | "\(.name) \(.manifest_path | rtrimstr("/Cargo.toml"))"')
CRATE_DIR=$(awk -v c="$CRATE" '$1 == c { print $2 }' <<< "$MEMBER_DIRS")
if [[ -z "$CRATE_DIR" ]]; then
    echo "::error::$CRATE is not a workspace crate"
    exit 1
fi

# cargo-cyclonedx writes a file next to every workspace member's Cargo.toml.
NAME=sbom-tmp
cargo cyclonedx --format json --spec-version 1.5 --all --override-filename "$NAME" "$@"
mv "$CRATE_DIR/$NAME.json" "$OUT"
awk '{ print $2 }' <<< "$MEMBER_DIRS" | while read -r dir; do
    rm -f "$dir/$NAME.json"
done
