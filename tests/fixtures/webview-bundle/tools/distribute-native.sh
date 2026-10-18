#!/bin/sh
set -eu
for platform in darwin-arm64 linux-x64-gnu; do
  test -s "artifacts/fixture.$platform.node"
  cp "artifacts/fixture.$platform.node" "npm/$platform/binding.node"
done
