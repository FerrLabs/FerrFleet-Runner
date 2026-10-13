#!/usr/bin/env bash
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "${work}"' EXIT

python3 - "${root}/action.yml" "${work}/step.sh" <<'PY'
import sys, yaml
action = yaml.safe_load(open(sys.argv[1]))
step = next(s for s in action["runs"]["steps"] if s.get("id") == "run")
open(sys.argv[2], "w").write(step["run"])
PY

mkdir "${work}/bin"
cat > "${work}/bin/curl" <<'STUB'
#!/usr/bin/env bash
echo "curl $*" >> "${CALLS}"
while [ $# -gt 0 ]; do
  [ "$1" = -o ] && out="$2"
  shift
done
printf '{"run_id":"run-org","run_token":"org-run-token"}' > "${out}"
printf '201'
STUB
cat > "${work}/bin/runner" <<'STUB'
#!/usr/bin/env bash
{
  echo "runner $*"
  echo "env FERRFLEET_ORG_TOKEN=${FERRFLEET_ORG_TOKEN:-unset}"
  echo "env FERRFLEET_POOL_TOKEN=${FERRFLEET_POOL_TOKEN:-unset}"
  echo "env FERRFLEET_RUN_TOKEN=${FERRFLEET_RUN_TOKEN:-unset}"
} >> "${CALLS}"
if [ "${1:-}" = dispatch ]; then
  printf 'run-pool' > "${FERRFLEET_RUN_ID_FILE}"
fi
exit "${RUNNER_EXIT:-0}"
STUB
chmod +x "${work}/bin/"*

run_step() {
  : > "${work}/calls"
  : > "${work}/output"
  : > "${work}/summary"
  CALLS="${work}/calls" PATH="${work}/bin:${PATH}" \
    API_URL=https://api.test AGENT=pr-agent REPO=acme/api PR_NUMBER=7 \
    IMAGE=img MODE=binary FERRFLEET_RUNNER_BIN="${work}/bin/runner" \
    CLAUDE_CODE_OAUTH_TOKEN=claude RUNNER_TEMP="${work}" \
    GITHUB_OUTPUT="${work}/output" GITHUB_STEP_SUMMARY="${work}/summary" \
    FERRFLEET_ORG_TOKEN="$1" bash "${work}/step.sh"
}

fail() {
  echo "FAIL: $1" >&2
  cat "${work}/calls" >&2
  exit 1
}

run_step ffrp_pool_secret
grep -q '^runner dispatch pr-agent {"repo":"acme/api","pr_number":7}$' "${work}/calls" \
  || fail "pool token did not dispatch with the event context"
grep -q '^curl' "${work}/calls" && fail "pool token created a run with the org route"
grep -q '^env FERRFLEET_POOL_TOKEN=ffrp_pool_secret$' "${work}/calls" \
  || fail "the runner did not receive the pool token"
grep -q '^env FERRFLEET_ORG_TOKEN=unset$' "${work}/calls" \
  || fail "the pool token leaked as the org token variable"
grep -q '^run-id=run-pool$' "${work}/output" || fail "run-id output missing"

RUNNER_EXIT=4 run_step ffrp_pool_secret && fail "a failed run did not fail the step"
grep -q '^run-id=run-pool$' "${work}/output" || fail "run-id output lost on failure"

run_step org_token_secret
grep -q '^curl .*https://api.test/agents/pr-agent/runs' "${work}/calls" \
  || fail "org token did not create the run through the agents route"
grep -q '^runner $' "${work}/calls" || fail "org token path ran the runner with arguments"
grep -q '^env FERRFLEET_RUN_TOKEN=org-run-token$' "${work}/calls" \
  || fail "the run token did not reach the runner"
grep -q '^env FERRFLEET_ORG_TOKEN=unset$' "${work}/calls" \
  || fail "the org token reached the runner"
grep -q '^run-id=run-org$' "${work}/output" || fail "run-id output missing"

echo "action routing: ok"
