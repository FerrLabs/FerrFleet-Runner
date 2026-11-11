#!/usr/bin/env bash
set -euo pipefail

image=${1:?usage: mcp_grafana_smoke.sh <runner image>}
grafana_image=grafana/grafana:12.4.14@sha256:ae509d9ae8c1021ca32ec370a9ca975d40165eda9fccd1e1102d317ff8787cd7
suffix=$$
network=mcp-grafana-smoke-${suffix}
grafana=grafana-${suffix}
admin_password=smoke-admin-password
datasource_name=smoke-datasource

cleanup() {
  docker rm -f "${grafana}" >/dev/null 2>&1 || true
  docker network rm "${network}" >/dev/null 2>&1 || true
}
trap cleanup EXIT

in_network() {
  docker run --rm --network "${network}" --entrypoint "$@"
}

grafana_api() {
  local method=$1 path=$2 body=${3:-}
  in_network curl "${image}" -fsS -X "${method}" \
    -u "admin:${admin_password}" -H 'Content-Type: application/json' \
    ${body:+--data "${body}"} "http://${grafana}:3000${path}"
}

docker network create "${network}" >/dev/null
docker run -d --name "${grafana}" --network "${network}" \
  -e "GF_SECURITY_ADMIN_PASSWORD=${admin_password}" "${grafana_image}" >/dev/null

for _ in $(seq 1 60); do
  if in_network curl "${image}" -fsS "http://${grafana}:3000/api/health" >/dev/null 2>&1; then
    break
  fi
  sleep 2
done
in_network curl "${image}" -fsS "http://${grafana}:3000/api/health" >/dev/null

grafana_api POST /api/datasources \
  "{\"name\":\"${datasource_name}\",\"type\":\"prometheus\",\"url\":\"http://prometheus:9090\",\"access\":\"proxy\"}" >/dev/null
account_id=$(grafana_api POST /api/serviceaccounts '{"name":"smoke","role":"Viewer"}' | jq -r .id)
token=$(grafana_api POST "/api/serviceaccounts/${account_id}/tokens" '{"name":"smoke"}' | jq -r .key)

requests=$(cat <<'JSON'
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"smoke","version":"0"}}}
{"jsonrpc":"2.0","method":"notifications/initialized"}
{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"list_datasources","arguments":{}}}
JSON
)

responses=$({ printf '%s\n' "${requests}"; sleep 10; } | GRAFANA_SERVICE_ACCOUNT_TOKEN=${token} docker run -i --rm \
  --network "${network}" \
  -e "GRAFANA_URL=http://${grafana}:3000" \
  -e GRAFANA_SERVICE_ACCOUNT_TOKEN \
  --entrypoint mcp-grafana "${image}" --disable-write)

printf '%s\n' "${responses}" | jq -c .

printf '%s\n' "${responses}" | jq -e -s '.[] | select(.id == 1) | .result.serverInfo.name' >/dev/null || {
  echo "::error::mcp-grafana did not answer initialize over stdio"
  exit 1
}
printf '%s\n' "${responses}" | jq -e -s --arg name "${datasource_name}" \
  '[.[] | select(.id == 2) | select(.result.isError != true) | .result.content[].text | select(contains($name))] | length > 0' >/dev/null || {
  echo "::error::list_datasources did not return the provisioned datasource"
  exit 1
}
