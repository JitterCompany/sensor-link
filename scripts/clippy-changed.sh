#!/usr/bin/env bash
# Run clippy over the whole workspace, but only fail on findings in files that
# changed relative to a base ref. This ratchets down the pre-existing backlog of
# findings incrementally: touch a file, and you own its clippy findings.
#
# Compile errors always fail, regardless of which files changed.
#
# Usage: ./scripts/clippy-changed.sh [base-ref]   (default: origin/master)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

BASE_REF="${1:-origin/master}"

for cmd in cargo git jq; do
    command -v "$cmd" >/dev/null || { echo "Required command not found: $cmd" >&2; exit 1; }
done

MERGE_BASE="$(git merge-base "$BASE_REF" HEAD)"

# Deleted files can't carry findings, so exclude them. Paths are relative to the
# repo root, which matches the file_name cargo reports for workspace members.
# Read loop rather than mapfile: macOS still ships bash 3.2.
CHANGED_FILES=()
while IFS= read -r file; do
    CHANGED_FILES+=("$file")
done < <(git diff --name-only --diff-filter=d "$MERGE_BASE" HEAD -- '*.rs')

echo "Base ref:    $BASE_REF ($MERGE_BASE)"
echo "Changed .rs: ${#CHANGED_FILES[@]} file(s)"

JSON="$(mktemp)"
CHANGED_JSON="$(mktemp)"
trap 'rm -f "$JSON" "$CHANGED_JSON"' EXIT

printf '%s\n' "${CHANGED_FILES[@]+"${CHANGED_FILES[@]}"}" | jq -R . | jq -s . > "$CHANGED_JSON"

# No -D warnings here: we decide what is fatal ourselves, below. Cargo still
# exits non-zero on a genuine compile error, which we always propagate.
CLIPPY_STATUS=0
cargo clippy --workspace --features sensor-link-firmware/test-mono \
    --message-format=json > "$JSON" || CLIPPY_STATUS=$?

# A diagnostic belongs to a file if its *primary* span points there, so a
# finding is attributed to the code at fault rather than to every file in the
# note/help chain.
FILTER='
  select(.reason == "compiler-message")
  | .message
  | select(.level == "warning" or .level == "error")
  | . as $msg
  | [.spans[] | select(.is_primary) | .file_name] as $primary
  | select($primary | any(. as $f | $changed | index($f) != null))
  | $msg.rendered
'

jq -r --argjson changed "$(cat "$CHANGED_JSON")" \
    "$FILTER" "$JSON"

RELEVANT=$(jq -rs --argjson changed "$(cat "$CHANGED_JSON")" \
    "[.[] | $FILTER] | length" "$JSON")

# Total across the workspace, for context on how much backlog is left.
TOTAL=$(jq -rs '[.[]
  | select(.reason == "compiler-message")
  | .message
  | select(.level == "warning" or .level == "error")] | length' "$JSON")

echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "Clippy findings in changed files: $RELEVANT (workspace total: $TOTAL)"

if [[ $CLIPPY_STATUS -ne 0 ]]; then
    echo "cargo clippy failed to compile the workspace" >&2
    exit "$CLIPPY_STATUS"
fi

if [[ $RELEVANT -gt 0 ]]; then
    echo "Fix the findings above, or justify them with a scoped #[allow(...)]." >&2
    exit 1
fi

echo "No clippy findings in changed files."
