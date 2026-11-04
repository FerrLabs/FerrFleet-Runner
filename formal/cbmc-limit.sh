#!/bin/sh
set -eu

margin=$((2 * 1024 * 1024 * 1024))
fallback=$((6 * 1024 * 1024 * 1024))

to_bytes() {
  case "$1" in
    *[Kk]) echo $((${1%?} * 1024)) ;;
    *[Mm]) echo $((${1%?} * 1024 * 1024)) ;;
    *[Gg]) echo $((${1%?} * 1024 * 1024 * 1024)) ;;
    *) echo "$1" ;;
  esac
}

default_limit() {
  max=$(cat /sys/fs/cgroup/memory.max 2>/dev/null || echo max)
  case "$max" in
    ''|*[!0-9]*) echo "$fallback" ;;
    *) if [ "$max" -gt "$margin" ]; then echo $((max - margin)); else echo "$fallback"; fi ;;
  esac
}

limit=$(to_bytes "${KANI_MEMORY_LIMIT:-$(default_limit)}")
exec prlimit --as="$limit" -- "$(dirname "$0")/cbmc.real" "$@"
