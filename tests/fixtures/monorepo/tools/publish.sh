#!/bin/sh
# Test recorder only. Never invokes a package manager or accesses the network.
set -eu
root=$(git rev-parse --show-toplevel)
package=$(git rev-parse --show-prefix)
printf '%s:%s\n' "$1" "${package%/}" >> "$root/.prp/publish.log"
if [ "${PRP_FIXTURE_FAIL:-}" = "$1:${package%/}" ]; then
  echo 'fixture publication failed' >&2
  exit 9
fi
