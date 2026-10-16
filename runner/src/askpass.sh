#!/bin/sh
url="${FERRFLEET_API_URL}/runs/${FERRFLEET_RUN_ID}/github-token"
delay=1
attempt=1
while :; do
  rc=0
  out=$(curl -s --connect-timeout 3 -w '\n%{http_code}' -H "Authorization: Bearer ${FERRFLEET_RUN_TOKEN}" "$url") || rc=$?
  status=$(printf '%s\n' "$out" | tail -n 1)
  if [ "$rc" -eq 0 ] && [ "$status" != 503 ]; then
    break
  fi
  if { [ "$rc" -ne 0 ] && [ "$rc" -ne 7 ] && [ "$rc" -ne 28 ]; } || [ "$attempt" -ge 5 ]; then
    exit 1
  fi
  sleep "$delay"
  delay=$((delay * 2))
  attempt=$((attempt + 1))
done
case "$status" in
  2??) ;;
  *) exit 1 ;;
esac
printf '%s\n' "$out" | sed '$d' | sed -n 's/.*"token"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p'
