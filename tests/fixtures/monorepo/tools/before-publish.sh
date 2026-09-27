#!/bin/sh
set -eu
test -f artifacts/bridge.node
root=$(git rev-parse --show-toplevel)
printf 'hook:packages/bridge\n' >> "$root/.prp/publish.log"
