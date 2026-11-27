#!/bin/sh
set -eu

gib=$((1024 * 1024 * 1024))
ceiling=$((1024 * gib))
fallback=$((6 * gib))
default_timeout=600
max_jobs=1024

die() {
  echo "cbmc-limit: $*" >&2
  exit 2
}

strip_zeros() {
  n=$1
  while :; do
    case "$n" in
      0?*) n=${n#0} ;;
      *) break ;;
    esac
  done
  echo "$n"
}

to_bytes() {
  case "$1" in
    ''|*[!0-9KkMmGgi]*) die "invalid KANI_MEMORY_LIMIT '$1'" ;;
  esac
  case "$1" in
    *[0-9]) digits=$1; mult=1 ;;
    *[Kk]) digits=${1%?}; mult=1024 ;;
    *[Kk]i) digits=${1%??}; mult=1024 ;;
    *[Mm]) digits=${1%?}; mult=$((1024 * 1024)) ;;
    *[Mm]i) digits=${1%??}; mult=$((1024 * 1024)) ;;
    *[Gg]) digits=${1%?}; mult=$gib ;;
    *[Gg]i) digits=${1%??}; mult=$gib ;;
    *) die "invalid KANI_MEMORY_LIMIT '$1'" ;;
  esac
  case "$digits" in
    ''|*[!0-9]*) die "invalid KANI_MEMORY_LIMIT '$1'" ;;
  esac
  digits=$(strip_zeros "$digits")
  [ "${#digits}" -le 13 ] || die "KANI_MEMORY_LIMIT '$1' is above the 1TiB ceiling"
  [ "$digits" -gt 0 ] || die "KANI_MEMORY_LIMIT '$1' must be positive"
  [ "$digits" -le $((ceiling / mult)) ] || die "KANI_MEMORY_LIMIT '$1' is above the 1TiB ceiling"
  echo $((digits * mult))
}

positive_int() {
  name=$1
  value=$2
  bound=$3
  case "$value" in
    ''|*[!0-9]*) die "invalid $name '$value'" ;;
  esac
  value=$(strip_zeros "$value")
  [ "${#value}" -le 9 ] || die "$name '$value' is above $bound"
  [ "$value" -gt 0 ] || die "$name '$value' must be positive"
  [ "$value" -le "$bound" ] || die "$name '$value' is above $bound"
  echo "$value"
}

cgroup_limit() {
  for file in /sys/fs/cgroup/memory.max /sys/fs/cgroup/memory/memory.limit_in_bytes; do
    [ -r "$file" ] || continue
    value=$(cat "$file" 2>/dev/null || true)
    case "$value" in
      ''|*[!0-9]*) continue ;;
    esac
    [ "${#value}" -le 15 ] || continue
    total_kb=$(awk '/^MemTotal:/ { print $2 }' /proc/meminfo 2>/dev/null || true)
    if [ -n "$total_kb" ] && [ "$value" -ge $((total_kb * 1024)) ]; then
      continue
    fi
    echo "$value"
    return
  done
}

default_limit() {
  max=$(cgroup_limit)
  if [ -z "$max" ]; then
    echo $((fallback / jobs))
  elif [ "$max" -gt $((8 * gib)) ]; then
    echo $(((max - 4 * gib) / jobs))
  else
    echo $((max / 2 / jobs))
  fi
}

jobs=$(positive_int KANI_JOBS "${KANI_JOBS:-1}" "$max_jobs")
timeout_seconds=$(positive_int KANI_CBMC_TIMEOUT "${KANI_CBMC_TIMEOUT:-$default_timeout}" 604800)

if [ -n "${KANI_MEMORY_LIMIT:-}" ]; then
  limit=$(to_bytes "$KANI_MEMORY_LIMIT")
else
  limit=$(default_limit)
fi
[ "$limit" -gt 0 ] || die "computed memory limit is not positive"

started=$(date +%s)
status=0
timeout -s KILL "$timeout_seconds" prlimit --as="$limit" -- "$(dirname "$0")/cbmc.real" "$@" || status=$?
if [ "$status" -eq 137 ] && [ $(($(date +%s) - started)) -ge "$timeout_seconds" ]; then
  echo "cbmc-limit: cbmc killed after ${timeout_seconds}s (KANI_CBMC_TIMEOUT)" >&2
fi
exit "$status"
