#!/usr/bin/env bash
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "${work}"' EXIT

gib=$((1 << 30))
mkdir "${work}/bin" "${work}/stub"

sed -e "s|/sys/fs/cgroup/memory.max|${work}/v2|" \
    -e "s|/sys/fs/cgroup/memory/memory.limit_in_bytes|${work}/v1|" \
    -e "s|/proc/meminfo|${work}/meminfo|" \
    "${root}/formal/cbmc-limit.sh" > "${work}/bin/cbmc"
chmod +x "${work}/bin/cbmc"

cat > "${work}/bin/cbmc.real" <<'STUB'
#!/bin/sh
for a in "$@"; do printf '%s\n' "$a"; done
if [ -n "${PIDS:-}" ]; then
  printf '%s\n' "$$" > "$PIDS"
  exec sleep 300
fi
exit "${CODE:-0}"
STUB
cat > "${work}/stub/prlimit" <<STUB
#!/bin/sh
printf '%s' "\${1#--as=}" > "${work}/as"
shift
[ "\$1" = -- ] && shift
exec "\$@"
STUB
chmod +x "${work}/bin/cbmc.real" "${work}/stub/prlimit"

failures=0
fail() { echo "FAIL: $*" >&2; failures=$((failures + 1)); }

run() {
  rm -f "${work}/as"
  env -u KANI_MEMORY_LIMIT -u KANI_JOBS -u KANI_CBMC_TIMEOUT PATH="${work}/stub:${PATH}" "$@" "${work}/bin/cbmc" x "y z" 2>"${work}/err" >"${work}/out" || return $?
}

setup() {
  rm -f "${work}/v2" "${work}/v1"
  echo "MemTotal: $((64 * 1024 * 1024)) kB" > "${work}/meminfo"
  [ -z "${1:-}" ] || printf '%s\n' "$1" > "${work}/v2"
  [ -z "${2:-}" ] || printf '%s\n' "$2" > "${work}/v1"
}

expect_cap() {
  local name=$1 want=$2
  shift 2
  local rc=0
  run "$@" || rc=$?
  [ "${rc}" -eq 0 ] || { fail "${name}: exit ${rc}"; return; }
  [ "$(cat "${work}/as")" = "${want}" ] || fail "${name}: cap $(cat "${work}/as") want ${want}"
}

expect_reject() {
  local name=$1
  shift
  local rc=0
  run "$@" || rc=$?
  [ "${rc}" -eq 2 ] || fail "${name}: exit ${rc}, want 2"
  [ ! -e "${work}/as" ] || fail "${name}: cbmc ran"
  [ -s "${work}/err" ] || fail "${name}: no message on stderr"
}

setup "$((8 * gib))"
expect_cap "8Gi pod" "$((4 * gib))"
setup "$((2 * gib))"
expect_cap "2Gi pod stays under limit" "$((1 * gib))"
setup "$((16 * gib))"
expect_cap "16Gi pod" "$((12 * gib))"
setup "$((12 * gib))"
expect_cap "12Gi pod leaves 4Gi to the rest of the pod" "$((8 * gib))"
setup "max"
expect_cap "unlimited v2" "$((6 * gib))"
setup
expect_cap "no cgroup" "$((6 * gib))"
setup "" "$((4 * gib))"
expect_cap "cgroup v1 4Gi" "$((2 * gib))"
setup "" "9223372036854771712"
expect_cap "cgroup v1 unlimited" "$((6 * gib))"
setup "" "$((128 * gib))"
expect_cap "cgroup v1 above MemTotal" "$((6 * gib))"

setup "$((8 * gib))"
for pair in "200M:$((200 << 20))" "200m:$((200 << 20))" "6G:$((6 * gib))" "6Gi:$((6 * gib))" \
            "512Ki:$((512 << 10))" "512k:$((512 << 10))" "300000000:300000000" "007G:$((7 * gib))" \
            "1024G:$((1024 * gib))"; do
  expect_cap "override ${pair%%:*}" "${pair#*:}" KANI_MEMORY_LIMIT="${pair%%:*}"
done
for bad in 0 00 0G -1 -1G +1 1.5G 6GB 6B G Gi ki abc "6 G" 99999999999G 1025G 99999999999999999999 1GiB 1kK 1Gg 1GG 1G2G 6KiK 1Kik 1mM 654m1M; do
  expect_reject "override '${bad}'" KANI_MEMORY_LIMIT="${bad}"
done

setup "$((16 * gib))"
expect_cap "jobs 1 is the default" "$((12 * gib))" KANI_JOBS=1
expect_cap "2 jobs split the budget" "$((6 * gib))" KANI_JOBS=2
expect_cap "3 jobs split the budget" "$((4 * gib))" KANI_JOBS=3
expect_cap "leading zeros in jobs" "$((6 * gib))" KANI_JOBS=002
setup "$((8 * gib))"
expect_cap "2 jobs in an 8Gi pod" "$((2 * gib))" KANI_JOBS=2
setup
expect_cap "2 jobs without a cgroup limit" "$((3 * gib))" KANI_JOBS=2
setup "$((16 * gib))"
expect_cap "jobs do not divide an explicit limit" "$((6 * gib))" KANI_JOBS=4 KANI_MEMORY_LIMIT=6G
for bad in 0 00 -1 +1 2.5 two "1 2" 1025 99999999999; do
  expect_reject "jobs '${bad}'" KANI_JOBS="${bad}"
done
for bad in 0 -1 1.5 10m abc 604801 99999999999; do
  expect_reject "timeout '${bad}'" KANI_CBMC_TIMEOUT="${bad}"
done

alive() {
  local state
  state=$(awk '{ sub(/^.*\) /, ""); print $1 }' "/proc/$1/stat" 2>/dev/null) || return 1
  [ "${state}" != Z ] && [ "${state}" != X ]
}

rc=0
started=$SECONDS
PIDS="${work}/pids" run KANI_CBMC_TIMEOUT=1 || rc=$?
[ "${rc}" -eq 137 ] || fail "runaway: exit ${rc}, want 137"
[ $((SECONDS - started)) -lt 10 ] || fail "runaway: took $((SECONDS - started))s"
grep -q 'killed after 1s (KANI_CBMC_TIMEOUT)' "${work}/err" || fail "runaway: no timeout message: $(cat "${work}/err")"
read -r cbmc_pid < "${work}/pids"
sleep 0.2
alive "${cbmc_pid}" && { fail "runaway: cbmc still alive"; kill -KILL "${cbmc_pid}"; }

mkdir "${work}/old"
printf '#!/bin/sh\nsleep 1\nexit 124\n' > "${work}/old/timeout"
chmod +x "${work}/old/timeout"
rc=0
PATH="${work}/old:${PATH}" run KANI_CBMC_TIMEOUT=1 || rc=$?
[ "${rc}" -eq 137 ] || fail "timeout reporting 124: exit ${rc}, want 137"
grep -q 'killed after 1s' "${work}/err" || fail "timeout reporting 124: no message"

rc=0
started=$SECONDS
CODE=0 run KANI_CBMC_TIMEOUT=30 || rc=$?
[ "${rc}" -eq 0 ] || fail "fast run: exit ${rc}"
[ $((SECONDS - started)) -lt 10 ] || fail "fast run: took $((SECONDS - started))s"
[ ! -s "${work}/err" ] || fail "fast run: unexpected stderr: $(cat "${work}/err")"
[ "$(cat "${work}/out")" = $'x\ny z' ] || fail "fast run: output changed"

rc=0
CODE=137 run KANI_CBMC_TIMEOUT=600 || rc=$?
[ "${rc}" -eq 137 ] || fail "a 137 from cbmc itself: ${rc}"
[ ! -s "${work}/err" ] || fail "a kill that is not the timeout was reported as one"

rc=0
CODE=37 run KANI_MEMORY_LIMIT=1G || rc=$?
[ "${rc}" -eq 37 ] || fail "exit status passthrough: ${rc}"
[ "$(cat "${work}/out")" = $'x\ny z' ] || fail "argument passthrough: $(cat "${work}/out")"

[ "${failures}" -eq 0 ] || exit 1
echo "cbmc-limit: all cases pass"
