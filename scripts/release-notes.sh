#!/usr/bin/env bash
# Write the CHANGELOG.md section for a version, the body of a GitHub release.
# Fails when the changelog has no "## <version>" section.
#
# Usage: ./scripts/release-notes.sh <CHANGELOG.md> <version> <out-file>

set -euo pipefail

CHANGELOG="$1"
VERSION="$2"
OUT="$3"

if [[ ! -f "$CHANGELOG" ]]; then
    echo "::error::$CHANGELOG not found"
    exit 1
fi

mkdir -p "$(dirname "$OUT")"
awk -v v="$VERSION" '/^## / { if (found) exit; found = ($2 == v); next } found' \
    "$CHANGELOG" > "$OUT"
if ! grep -q '[^[:space:]]' "$OUT"; then
    echo "::error::no '## $VERSION' section in $CHANGELOG"
    exit 1
fi
