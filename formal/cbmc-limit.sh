#!/bin/sh
set -eu

gib=$((1024 * 1024 * 1024))
ceiling=$((1024 * gib))
fallback=$((6 * gib))

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
    *[Kk]|*[Kk]i) digits=${1%%[Kk]*}; mult=1024 ;;
    *[Mm]|*[Mm]i) digits=${1%%[Mm]*}; mult=$((1024 * 1024)) ;;
    *[Gg]|*[Gg]i) digits=${1%%[Gg]*}; mult=$gib ;;
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
    echo "$fallback"
  elif [ "$max" -gt $((8 * gib)) ]; then
    echo $((max - 2 * gib))
  else
    echo $((max * 3 / 4))
  fi
}

if [ -n "${KANI_MEMORY_LIMIT:-}" ]; then
  limit=$(to_bytes "$KANI_MEMORY_LIMIT")
else
  limit=$(default_limit)
fi
[ "$limit" -gt 0 ] || die "computed memory limit is not positive"

exec prlimit --as="$limit" -- "$(dirname "$0")/cbmc.real" "$@"
