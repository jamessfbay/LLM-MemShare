#!/usr/bin/env bash
set -euo pipefail

PROJECT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
COMPOSE_FILE="$PROJECT_DIR/deploy/compose.multi-instance.yaml"
ARTIFACT_DIR="$PROJECT_DIR/.gstack/qa-reports"
REPORT="$ARTIFACT_DIR/multi-instance-$(date -u +%Y%m%dT%H%M%SZ).json"
TEMP_DIR="$(mktemp -d)"

export MEMSHARE_POSTGRES_ADMIN_PASSWORD="${MEMSHARE_POSTGRES_ADMIN_PASSWORD:-multi-admin-test-password}"
export MEMSHARE_POSTGRES_OWNER_PASSWORD="${MEMSHARE_POSTGRES_OWNER_PASSWORD:-multi-owner-test-password}"
export MEMSHARE_POSTGRES_RUNTIME_PASSWORD="${MEMSHARE_POSTGRES_RUNTIME_PASSWORD:-multi-runtime-test-password}"
export MEMSHARE_INSTANCE_1_PORT="${MEMSHARE_INSTANCE_1_PORT:-28081}"
export MEMSHARE_INSTANCE_2_PORT="${MEMSHARE_INSTANCE_2_PORT:-28082}"
export MEMSHARE_INSTANCE_3_PORT="${MEMSHARE_INSTANCE_3_PORT:-28083}"

TOKEN_A="multi-instance-tenant-a-token-0123456789abcdef"
URL_1="http://127.0.0.1:$MEMSHARE_INSTANCE_1_PORT"
URL_2="http://127.0.0.1:$MEMSHARE_INSTANCE_2_PORT"
URL_3="http://127.0.0.1:$MEMSHARE_INSTANCE_3_PORT"

cleanup() {
  if [[ "${MEMSHARE_KEEP_MULTI_INSTANCE_STACK:-0}" != "1" ]]; then
    docker compose -f "$COMPOSE_FILE" down -v >/dev/null 2>&1 || true
  fi
  if [[ "$TEMP_DIR" == /tmp/* || "$TEMP_DIR" == /private/tmp/* || "$TEMP_DIR" == /var/folders/* || "$TEMP_DIR" == /private/var/folders/* ]]; then
    rm -rf "$TEMP_DIR"
  fi
}
trap cleanup EXIT

for command in docker curl jq; do
  command -v "$command" >/dev/null || { echo "required command not found: $command" >&2; exit 1; }
done

mkdir -p "$ARTIFACT_DIR"

compose() {
  docker compose -f "$COMPOSE_FILE" "$@"
}

request() {
  local method="$1"
  local url="$2"
  local token="$3"
  local output="$4"
  local body="${5:-}"
  local key="${6:-}"
  local args=(-sS -o "$output" -w '%{http_code}' -X "$method" "$url" -H "Authorization: Bearer $token")
  if [[ -n "$body" ]]; then
    args+=(-H 'Content-Type: application/json' -d "$body")
  fi
  if [[ -n "$key" ]]; then
    args+=(-H "Idempotency-Key: $key")
  fi
  curl "${args[@]}"
}

expect_status() {
  local actual="$1"
  local expected="$2"
  local label="$3"
  if [[ "$actual" != "$expected" ]]; then
    echo "$label: expected HTTP $expected, got $actual" >&2
    exit 1
  fi
}

echo "[1/9] Building the Rust image"
compose build migrate >/dev/null

echo "[2/9] Starting PostgreSQL, migration, and three service instances"
compose up -d >/dev/null
for url in "$URL_1" "$URL_2" "$URL_3"; do
  ready=0
  for _ in $(seq 1 40); do
    if curl -sS --fail "$url/health/ready" >/dev/null 2>&1; then
      ready=1
      break
    fi
    sleep 1
  done
  [[ "$ready" == "1" ]] || { echo "instance did not become ready: $url" >&2; exit 1; }
done

echo "[3/9] Bootstrapping two isolated tenants"
bootstrap_a="$(compose run --rm -e MEMSHARE_BOOTSTRAP_TOKEN="$TOKEN_A" migrate bootstrap --tenant tenant-a --workspace main --user owner-a)"
workspace_a="$(printf '%s\n' "$bootstrap_a" | sed -n 's/^workspace_id=//p')"
principal_a="$(printf '%s\n' "$bootstrap_a" | sed -n 's/^principal_id=//p')"
tenant_b="$(compose run --rm migrate provision-tenant --tenant tenant-b --workspace main --user owner-b)"
token_b="$(printf '%s\n' "$tenant_b" | sed -n 's/^token=//p')"

[[ -n "$workspace_a" && -n "$principal_a" && -n "$token_b" ]] || { echo "tenant provisioning output was incomplete" >&2; exit 1; }

memory_body="$(jq -cn --arg workspace "$workspace_a" --arg principal "$principal_a" '{workspace_id:$workspace,project_id:null,scope:"private",scope_id:$principal,kind:"fact",content:"shared-database multi-instance memory",source:{suite:"multi-instance"},expires_at:null}')"

echo "[4/9] Verifying cross-instance idempotency"
status_create="$(request POST "$URL_1/v1/memories" "$TOKEN_A" "$TEMP_DIR/create.json" "$memory_body" multi-instance-create)"
expect_status "$status_create" 201 "create through instance 1"
memory_id="$(jq -r '.id' "$TEMP_DIR/create.json")"
status_replay="$(request POST "$URL_2/v1/memories" "$TOKEN_A" "$TEMP_DIR/replay.json" "$memory_body" multi-instance-create)"
expect_status "$status_replay" 200 "replay through instance 2"
[[ "$(jq -r '.id' "$TEMP_DIR/replay.json")" == "$memory_id" ]] || { echo "idempotency returned different resources across instances" >&2; exit 1; }

collision_body="$(printf '%s' "$memory_body" | jq -c '.content="idempotency collision"')"
status_collision="$(request POST "$URL_3/v1/memories" "$TOKEN_A" "$TEMP_DIR/collision.json" "$collision_body" multi-instance-create)"
expect_status "$status_collision" 409 "collision through instance 3"

echo "[5/9] Verifying cross-instance write serialization"
patch_one='{"version":1,"content":"writer one"}'
patch_two='{"version":1,"content":"writer two"}'
request PATCH "$URL_2/v1/memories/$memory_id" "$TOKEN_A" "$TEMP_DIR/write-1.json" "$patch_one" >"$TEMP_DIR/write-1.status" &
pid_one=$!
request PATCH "$URL_3/v1/memories/$memory_id" "$TOKEN_A" "$TEMP_DIR/write-2.json" "$patch_two" >"$TEMP_DIR/write-2.status" &
pid_two=$!
wait "$pid_one"
wait "$pid_two"
status_one="$(tr -d '[:space:]' <"$TEMP_DIR/write-1.status")"
status_two="$(tr -d '[:space:]' <"$TEMP_DIR/write-2.status")"
sorted_statuses="$(printf '%s\n%s\n' "$status_one" "$status_two" | sort | tr '\n' ' ')"
[[ "$sorted_statuses" == "200 409 " ]] || { echo "concurrent writes must produce one 200 and one 409; got $sorted_statuses" >&2; exit 1; }

echo "[6/9] Verifying grants and immediate revocation propagation"
status_agent="$(request POST "$URL_1/v1/agents" "$TOKEN_A" "$TEMP_DIR/agent.json" "$(jq -cn --arg workspace "$workspace_a" '{name:"multi-reader",workspace_id:$workspace,expires_at:null}')")"
expect_status "$status_agent" 201 "agent creation"
agent_id="$(jq -r '.principal_id' "$TEMP_DIR/agent.json")"
agent_token="$(jq -r '.token' "$TEMP_DIR/agent.json")"
grant_body="$(jq -cn --arg agent "$agent_id" --arg workspace "$workspace_a" --arg owner "$principal_a" '{subject_principal_id:$agent,workspace_id:$workspace,scope:"private",scope_id:$owner,actions:["read"],expires_at:null}')"
status_grant="$(request POST "$URL_2/v1/grants" "$TOKEN_A" "$TEMP_DIR/grant.json" "$grant_body")"
expect_status "$status_grant" 201 "grant creation through instance 2"
grant_id="$(jq -r '.id' "$TEMP_DIR/grant.json")"
status_agent_read="$(request GET "$URL_3/v1/memories/$memory_id" "$agent_token" "$TEMP_DIR/agent-read.json")"
expect_status "$status_agent_read" 200 "agent read through instance 3"
status_revoke="$(request DELETE "$URL_1/v1/grants/$grant_id" "$TOKEN_A" "$TEMP_DIR/revoke.json")"
expect_status "$status_revoke" 200 "grant revoke through instance 1"
status_after_revoke="$(request GET "$URL_2/v1/memories/$memory_id" "$agent_token" "$TEMP_DIR/after-revoke.json")"
expect_status "$status_after_revoke" 404 "revocation observed through instance 2"

echo "[7/9] Verifying cross-agent and cross-LLM handoff"
status_source_agent="$(request POST "$URL_1/v1/agents" "$TOKEN_A" "$TEMP_DIR/source-agent.json" "$(jq -cn --arg workspace "$workspace_a" '{name:"openai-source",workspace_id:$workspace,expires_at:null}')")"
expect_status "$status_source_agent" 201 "source agent creation"
source_agent_id="$(jq -r '.principal_id' "$TEMP_DIR/source-agent.json")"
source_agent_token="$(jq -r '.token' "$TEMP_DIR/source-agent.json")"
status_target_agent="$(request POST "$URL_2/v1/agents" "$TOKEN_A" "$TEMP_DIR/target-agent.json" "$(jq -cn --arg workspace "$workspace_a" '{name:"anthropic-target",workspace_id:$workspace,expires_at:null}')")"
expect_status "$status_target_agent" 201 "target agent creation"
target_agent_id="$(jq -r '.principal_id' "$TEMP_DIR/target-agent.json")"
target_agent_token="$(jq -r '.token' "$TEMP_DIR/target-agent.json")"

source_read_grant="$(jq -cn --arg source "$source_agent_id" --arg workspace "$workspace_a" --arg owner "$principal_a" '{subject_principal_id:$source,workspace_id:$workspace,scope:"private",scope_id:$owner,actions:["read"],expires_at:null}')"
status_source_read="$(request POST "$URL_3/v1/grants" "$TOKEN_A" "$TEMP_DIR/source-read-grant.json" "$source_read_grant")"
expect_status "$status_source_read" 201 "source memory grant"
handoff_grant="$(jq -cn --arg source "$source_agent_id" --arg target "$target_agent_id" --arg workspace "$workspace_a" '{subject_principal_id:$source,workspace_id:$workspace,scope:"agent",scope_id:$target,actions:["handoff"],expires_at:null}')"
status_handoff_grant="$(request POST "$URL_3/v1/grants" "$TOKEN_A" "$TEMP_DIR/handoff-grant.json" "$handoff_grant")"
expect_status "$status_handoff_grant" 201 "directed handoff grant"

handoff_arguments="$(jq -cn --arg workspace "$workspace_a" --arg target "$target_agent_id" --arg memory "$memory_id" '{workspace_id:$workspace,target_agent_id:$target,task:{summary:"Review the shared deployment state",protocol_version:"1.0",instructions:null,data:{}},input_memory_ids:[$memory],source:{provider:"openai",model:"deterministic-openai-test",framework:"responses"},expires_at:null}')"
openai_call="$(jq -cn --arg arguments "$handoff_arguments" '{type:"function_call",name:"memshare_create_handoff",arguments:$arguments,call_id:"call_multi_instance_1"}')"
status_handoff_create="$(request POST "$URL_1/v1/adapters/openai/handoffs" "$source_agent_token" "$TEMP_DIR/handoff-create.json" "$openai_call" multi-instance-handoff)"
expect_status "$status_handoff_create" 201 "OpenAI-adapted handoff creation"
handoff_id="$(jq -r '.id' "$TEMP_DIR/handoff-create.json")"
[[ "$(jq -r '.status' "$TEMP_DIR/handoff-create.json")" == "queued" ]] || { echo "handoff was not queued" >&2; exit 1; }

status_handoff_claim="$(request POST "$URL_2/v1/handoffs/$handoff_id/claim" "$target_agent_token" "$TEMP_DIR/handoff-claim.json" '{"version":1}')"
expect_status "$status_handoff_claim" 200 "target claim through instance 2"
[[ "$(jq -r '.omitted_input_references' "$TEMP_DIR/handoff-claim.json")" == "1" ]] || { echo "target must not receive an unauthorized memory reference" >&2; exit 1; }

anthropic_call='{"type":"tool_use","name":"memshare_complete_handoff","input":{"version":2,"result":{"protocol_version":"1.0","summary":"Cross-model review complete","outcome":"succeeded","data":{"accepted":true}},"result_memory_ids":[],"source":{"provider":"anthropic","model":"deterministic-anthropic-test","framework":"messages"}},"id":"toolu_multi_instance_1"}'
status_handoff_complete="$(request POST "$URL_3/v1/adapters/anthropic/handoffs/$handoff_id/complete" "$target_agent_token" "$TEMP_DIR/handoff-complete.json" "$anthropic_call")"
expect_status "$status_handoff_complete" 200 "Anthropic-adapted handoff completion"
[[ "$(jq -r '.status' "$TEMP_DIR/handoff-complete.json")" == "completed" ]] || { echo "handoff was not completed" >&2; exit 1; }
status_source_result="$(request GET "$URL_1/v1/handoffs/$handoff_id" "$source_agent_token" "$TEMP_DIR/handoff-source-result.json")"
expect_status "$status_source_result" 200 "source result read through instance 1"
[[ "$(jq -r '.result.accepted' "$TEMP_DIR/handoff-source-result.json")" == "true" ]] || { echo "source did not receive the target result" >&2; exit 1; }

echo "[8/9] Verifying tenant isolation through another instance"
status_cross_tenant="$(request GET "$URL_3/v1/memories/$memory_id" "$token_b" "$TEMP_DIR/cross-tenant.json")"
expect_status "$status_cross_tenant" 404 "cross-tenant ID guess"
status_cross_tenant_handoff="$(request GET "$URL_2/v1/handoffs/$handoff_id" "$token_b" "$TEMP_DIR/cross-tenant-handoff.json")"
expect_status "$status_cross_tenant_handoff" 404 "cross-tenant handoff ID guess"

echo "[9/9] Writing verification artifact"
jq -n \
  --arg timestamp "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
  --arg memory_id "$memory_id" \
  --arg handoff_id "$handoff_id" \
  --arg concurrent_statuses "$sorted_statuses" \
  '{status:"passed",instances:3,agents:2,llm_adapters:["openai-function-call","anthropic-tool-use"],database:"shared-postgresql",checks:["health","migration","two-tenant-provisioning","cross-instance-idempotency","idempotency-collision","concurrent-version-serialization","grant-propagation","revocation-propagation","directed-agent-handoff-grant","openai-adapted-handoff-create","target-agent-claim","unauthorized-reference-omission","anthropic-adapted-handoff-complete","source-agent-result-read","cross-tenant-isolation"],memory_id:$memory_id,handoff_id:$handoff_id,concurrent_statuses:$concurrent_statuses,timestamp:$timestamp}' > "$REPORT"

echo "Multi-instance verification passed"
echo "Report: $REPORT"
