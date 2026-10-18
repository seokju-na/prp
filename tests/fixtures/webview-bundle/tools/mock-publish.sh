#!/bin/sh
# Execute the Rust test helper, never a real package manager.
set -eu
: "${PRP_MOCK_EXECUTABLE:?Only the E2E harness can run this publisher}"
export PRP_MOCK_REGISTRY="$1"
shift
# One argument per line; fixture arguments deliberately contain no newlines.
PRP_MOCK_ARGS=$(printf '%s\n' "$@")
export PRP_MOCK_ARGS
exec "$PRP_MOCK_EXECUTABLE" --exact mock_publish --ignored --nocapture
