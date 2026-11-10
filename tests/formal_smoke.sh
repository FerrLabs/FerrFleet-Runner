#!/usr/bin/env bash
set -euo pipefail

image=${1:?usage: formal_smoke.sh <image>}
root=$(cd "$(dirname "$0")" && pwd)

log=$(mktemp)
trap 'rm -f "${log}"' EXIT

docker run --rm --read-only --tmpfs /tmp:exec --tmpfs /workdir:exec,mode=1777 -u 1000 -e HOME=/tmp -e RUSTC_WRAPPER=sccache -e SCCACHE_DIR=/tmp/sccache -e CARGO_INCREMENTAL=0 -w /workdir \
  --entrypoint sh -v "${root}/formal-smoke:/smoke:ro" "${image}" -c '
    set -eu
    cp -r /smoke/. /workdir/
    sccache --version
    cargo build
    cargo test
    cargo kani
    sccache --show-stats | grep -Eq "Compile requests executed +[1-9]"
    if [ -e /tmp/.kani ]; then echo "kani fetched a bundle under HOME" >&2; exit 1; fi
    ls /home/runner/.kani/kani-*/bin/cbmc.real >/dev/null
  ' 2>&1 | tee "${log}"

if grep -q "Read-only file system" "${log}"; then
  echo "read-only filesystem error" >&2
  exit 1
fi
if grep -Eq "info: (downloading|syncing channel|installing component)" "${log}"; then
  echo "toolchain download at run time" >&2
  exit 1
fi
if ! grep -q "VERIFICATION:- SUCCESSFUL" "${log}"; then
  echo "kani did not report a successful verification" >&2
  exit 1
fi
