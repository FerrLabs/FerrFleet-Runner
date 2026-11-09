#!/usr/bin/env bash
set -euo pipefail

image=${1:?usage: formal_smoke.sh <image>}
root=$(cd "$(dirname "$0")" && pwd)

log=$(mktemp)
trap 'rm -f "${log}"' EXIT

docker run --rm --read-only --tmpfs /tmp:exec --tmpfs /workdir:exec -u 1000 -w /workdir \
  --entrypoint sh -v "${root}/formal-smoke:/smoke:ro" "${image}" -c '
    set -eu
    cp -r /smoke/. /workdir/
    cargo build
    cargo test
    cargo kani
  ' 2>&1 | tee "${log}"

if grep -q "Read-only file system" "${log}"; then
  echo "read-only filesystem error" >&2
  exit 1
fi
if grep -Eq "info: (downloading|syncing channel|installing component)" "${log}"; then
  echo "toolchain download at run time" >&2
  exit 1
fi
grep -q "VERIFICATION:- SUCCESSFUL" "${log}"
