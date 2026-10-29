#!/usr/bin/env bash
set -euo pipefail

PROJECT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CONTROL_URL="${MEMSHARE_TEST_DATABASE_URL:?Set MEMSHARE_TEST_DATABASE_URL to a disposable PostgreSQL control database}"
TEST_HOST="${MEMSHARE_TEST_HOST:-127.0.0.1}"
TEST_PORT="${MEMSHARE_TEST_PORT:-5432}"
PORT_1="${MEMSHARE_INSTANCE_1_PORT:-28081}"
PORT_2="${MEMSHARE_INSTANCE_2_PORT:-28082}"
PORT_3="${MEMSHARE_INSTANCE_3_PORT:-28083}"
RUN_ID="$(date -u +%Y%m%d%H%M%S)-$$"
DATABASE_NAME="memshare_multiprocess_${RUN_ID//-/_}"
RUNTIME_ROLE="memshare_runtime_${RUN_ID//-/_}"
RUNTIME_PASSWORD="runtime_${RUN_ID//-/_}_password"
OWNER_URL="${CONTROL_URL%/*}/$DATABASE_NAME"
RUNTIME_URL="postgresql://$RUNTIME_ROLE:$RUNTIME_PASSWORD@$TEST_HOST:$TEST_PORT/$DATABASE_NAME"
TEMP_DIR="$(mktemp -d)"
REPORT_DIR="$PROJECT_DIR/.gstack/qa-reports"
REPORT="$REPORT_DIR/multi-process-$(date -u +%Y%m%dT%H%M%SZ).json"
TOKEN_A="multi-process-tenant-a-token-0123456789abcdef"
PIDS=()

cleanup() {
  for pid in "${PIDS[@]:-}"; do
    if [[ "$pid" =~ ^[0-9]+$ ]]; then
      kill "$pid" >/dev/null 2>&1 || true
      wait "$pid" >/dev/null 2>&1 || true
    fi
  done
  if [[ "$DATABASE_NAME" == memshare_multiprocess_* ]]; then
    psql "$CONTROL_URL" -v ON_ERROR_STOP=1 -c "DROP DATABASE IF EXISTS \"$DATABASE_NAME\" WITH (FORCE)" >/dev/null 2>&1 || true
  fi
  if [[ "$RUNTIME_ROLE" == memshare_runtime_* ]]; then
    psql "$CONTROL_URL" -v ON_ERROR_STOP=1 -c "DROP ROLE IF EXISTS \"$RUNTIME_ROLE\"" >/dev/null 2>&1 || true
  fi
  if [[ "$TEMP_DIR" == /tmp/* || "$TEMP_DIR" == /private/tmp/* || "$TEMP_DIR" == /var/folders/* || "$TEMP_DIR" == /private/var/folders/* ]]; then
    find "$TEMP_DIR" -depth -delete 2>/dev/null || true
  fi
}
trap cleanup EXIT

for command in cargo curl jq psql; do
  command -v "$command" >/dev/null || { echo "required command not found: $command" >&2; exit 1; }
done

mkdir -p "$REPORT_DIR"
cargo build --locked >/dev/null
BINARY="$PROJECT_DIR/target/debug/llm-memshare"

psql "$CONTROL_URL" -v ON_ERROR_STOP=1 -c "CREATE DATABASE \"$DATABASE_NAME\"" >/dev/null
psql "$CONTROL_URL" -v ON_ERROR_STOP=1 -c "CREATE ROLE \"$RUNTIME_ROLE\" LOGIN NOSUPERUSER NOBYPASSRLS NOCREATEDB NOCREATEROLE PASSWORD '$RUNTIME_PASSWORD'" >/dev/null

MEMSHARE_DATABASE_URL="$OWNER_URL" "$BINARY" migrate
psql "$OWNER_URL" -v ON_ERROR_STOP=1 \
  -c "GRANT CONNECT ON DATABASE \"$DATABASE_NAME\" TO \"$RUNTIME_ROLE\"" \
  -c "GRANT USAGE ON SCHEMA memshare TO \"$RUNTIME_ROLE\"" \
  -c "GRANT SELECT,INSERT,UPDATE,DELETE ON ALL TABLES IN SCHEMA memshare TO \"$RUNTIME_ROLE\"" \
  -c "GRANT EXECUTE ON FUNCTION memshare.authenticate_request(text) TO \"$RUNTIME_ROLE\"" >/dev/null

bootstrap_output="$(MEMSHARE_DATABASE_URL="$OWNER_URL" MEMSHARE_BOOTSTRAP_TOKEN="$TOKEN_A" "$BINARY" bootstrap --tenant tenant-a --workspace main --user owner-a)"
workspace_a="$(printf '%s\n' "$bootstrap_output" | sed -n 's/^workspace_id=//p')"
principal_a="$(printf '%s\n' "$bootstrap_output" | sed -n 's/^principal_id=//p')"
tenant_b_output="$(MEMSHARE_DATABASE_URL="$OWNER_URL" "$BINARY" provision-tenant --tenant tenant-b --workspace main --user owner-b)"
token_b="$(printf '%s\n' "$tenant_b_output" | sed -n 's/^token=//p')"
[[ -n "$workspace_a" && -n "$principal_a" && -n "$token_b" ]] || { echo "tenant provisioning failed" >&2; exit 1; }

for port in "$PORT_1" "$PORT_2" "$PORT_3"; do
  MEMSHARE_DATABASE_URL="$RUNTIME_URL" MEMSHARE_BIND="127.0.0.1:$port" \
    MEMSHARE_PUBLIC_BASE_URL="http://127.0.0.1:$port" \
    "$BINARY" serve >"$TEMP_DIR/$port.log" 2>&1 &
  PIDS+=("$!")
done

for port in "$PORT_1" "$PORT_2" "$PORT_3"; do
  ready=0
  for _ in $(seq 1 50); do
    if curl -sS --fail "http://127.0.0.1:$port/health/ready" >/dev/null 2>&1; then
      ready=1
      break
    fi
    sleep 0.2
  done
  if [[ "$ready" != 1 ]]; then
    sed -n '1,120p' "$TEMP_DIR/$port.log" >&2
    echo "instance on port $port did not become ready" >&2
    exit 1
  fi
done

memory_body="$(jq -cn --arg workspace "$workspace_a" --arg principal "$principal_a" '{workspace_id:$workspace,project_id:null,scope:"private",scope_id:$principal,kind:"fact",content:"non-Docker multi-process memory",source:{suite:"multi-process"},expires_at:null}')"
create_status="$(curl -sS -o "$TEMP_DIR/create.json" -w '%{http_code}' -X POST "http://127.0.0.1:$PORT_1/v1/memories" -H "Authorization: Bearer $TOKEN_A" -H 'Content-Type: application/json' -H 'Idempotency-Key: multi-process-memory' -d "$memory_body")"
[[ "$create_status" == 201 ]] || { echo "memory create failed: $create_status" >&2; exit 1; }
memory_id="$(jq -r '.id' "$TEMP_DIR/create.json")"
replay_status="$(curl -sS -o "$TEMP_DIR/replay.json" -w '%{http_code}' -X POST "http://127.0.0.1:$PORT_2/v1/memories" -H "Authorization: Bearer $TOKEN_A" -H 'Content-Type: application/json' -H 'Idempotency-Key: multi-process-memory' -d "$memory_body")"
[[ "$replay_status" == 200 && "$(jq -r '.id' "$TEMP_DIR/replay.json")" == "$memory_id" ]] || { echo "cross-process idempotency failed" >&2; exit 1; }

list_status="$(curl -sS -o "$TEMP_DIR/list.json" -w '%{http_code}' "http://127.0.0.1:$PORT_3/v1/memories?limit=1" -H "Authorization: Bearer $TOKEN_A")"
[[ "$list_status" == 200 && "$(jq -r '.items[0].id' "$TEMP_DIR/list.json")" == "$memory_id" ]] || { echo "cross-process read failed" >&2; exit 1; }
cross_tenant_status="$(curl -sS -o "$TEMP_DIR/cross-tenant.json" -w '%{http_code}' "http://127.0.0.1:$PORT_2/v1/memories/$memory_id" -H "Authorization: Bearer $token_b")"
[[ "$cross_tenant_status" == 404 ]] || { echo "cross-tenant isolation failed" >&2; exit 1; }

agent_source_body="$(jq -cn --arg workspace "$workspace_a" '{name:"source-agent",workspace_id:$workspace,expires_at:null}')"
agent_target_body="$(jq -cn --arg workspace "$workspace_a" '{name:"target-agent",workspace_id:$workspace,expires_at:null}')"
source_status="$(curl -sS -o "$TEMP_DIR/source-agent.json" -w '%{http_code}' -X POST "http://127.0.0.1:$PORT_1/v1/agents" -H "Authorization: Bearer $TOKEN_A" -H 'Content-Type: application/json' -d "$agent_source_body")"
target_status="$(curl -sS -o "$TEMP_DIR/target-agent.json" -w '%{http_code}' -X POST "http://127.0.0.1:$PORT_2/v1/agents" -H "Authorization: Bearer $TOKEN_A" -H 'Content-Type: application/json' -d "$agent_target_body")"
[[ "$source_status" == 201 && "$target_status" == 201 ]] || { echo "cross-process agent creation failed" >&2; exit 1; }
source_id="$(jq -r '.principal_id' "$TEMP_DIR/source-agent.json")"
source_token="$(jq -r '.token' "$TEMP_DIR/source-agent.json")"
target_id="$(jq -r '.principal_id' "$TEMP_DIR/target-agent.json")"
target_token="$(jq -r '.token' "$TEMP_DIR/target-agent.json")"
grant_body="$(jq -cn --arg subject "$source_id" --arg workspace "$workspace_a" --arg target "$target_id" '{subject_principal_id:$subject,workspace_id:$workspace,scope:"agent",scope_id:$target,actions:["handoff"],expires_at:null}')"
grant_status="$(curl -sS -o "$TEMP_DIR/grant.json" -w '%{http_code}' -X POST "http://127.0.0.1:$PORT_3/v1/grants" -H "Authorization: Bearer $TOKEN_A" -H 'Content-Type: application/json' -d "$grant_body")"
[[ "$grant_status" == 201 ]] || { echo "cross-process handoff grant failed" >&2; exit 1; }
handoff_body="$(jq -cn --arg workspace "$workspace_a" --arg target "$target_id" '{workspace_id:$workspace,target_agent_id:$target,task:{protocol_version:"1.0",summary:"three-process handoff",instructions:null,data:{}},input_memory_ids:[],source:{provider:"system-test",model:"source",framework:"shell"},expires_at:null}')"
handoff_status="$(curl -sS -o "$TEMP_DIR/handoff.json" -w '%{http_code}' -X POST "http://127.0.0.1:$PORT_1/v1/handoffs" -H "Authorization: Bearer $source_token" -H 'Content-Type: application/json' -H 'Idempotency-Key: three-process-handoff' -d "$handoff_body")"
[[ "$handoff_status" == 201 ]] || { echo "cross-process handoff creation failed" >&2; exit 1; }
handoff_id="$(jq -r '.id' "$TEMP_DIR/handoff.json")"
claim_status="$(curl -sS -o "$TEMP_DIR/claim.json" -w '%{http_code}' -X POST "http://127.0.0.1:$PORT_2/v1/handoffs/$handoff_id/claim" -H "Authorization: Bearer $target_token" -H 'Content-Type: application/json' -d '{"version":1,"lease_seconds":60}')"
[[ "$claim_status" == 200 && "$(jq -r '.version' "$TEMP_DIR/claim.json")" == 2 ]] || { echo "cross-process handoff claim failed" >&2; exit 1; }
complete_body='{"version":2,"result":{"protocol_version":"1.0","summary":"completed on third process","outcome":"succeeded","data":{}},"result_memory_ids":[],"source":{"provider":"system-test","model":"target","framework":"shell"}}'
complete_status="$(curl -sS -o "$TEMP_DIR/complete.json" -w '%{http_code}' -X POST "http://127.0.0.1:$PORT_3/v1/handoffs/$handoff_id/complete" -H "Authorization: Bearer $target_token" -H 'Content-Type: application/json' -d "$complete_body")"
[[ "$complete_status" == 200 && "$(jq -r '.status' "$TEMP_DIR/complete.json")" == completed ]] || { echo "cross-process handoff completion failed" >&2; exit 1; }

mcp_status="$(curl -sS -o "$TEMP_DIR/mcp.json" -w '%{http_code}' -X POST "http://127.0.0.1:$PORT_1/v1/mcp" -H "Authorization: Bearer $TOKEN_A" -H 'Content-Type: application/json' -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"system-test","version":"1"}}}')"
[[ "$mcp_status" == 200 && "$(jq -r '.result.protocolVersion' "$TEMP_DIR/mcp.json")" == "2025-06-18" ]] || { echo "MCP initialization failed" >&2; exit 1; }
a2a_status="$(curl -sS -o "$TEMP_DIR/agent-card.json" -w '%{http_code}' "http://127.0.0.1:$PORT_2/.well-known/agent-card.json")"
[[ "$a2a_status" == 200 && "$(jq -r '.protocolVersion' "$TEMP_DIR/agent-card.json")" == "0.2.6" ]] || { echo "A2A Agent Card failed" >&2; exit 1; }

admin_status="$(curl -sS -o "$TEMP_DIR/principals.json" -w '%{http_code}' "http://127.0.0.1:$PORT_3/v1/admin/principals?limit=10" -H "Authorization: Bearer $TOKEN_A")"
[[ "$admin_status" == 200 && "$(jq '.items | length' "$TEMP_DIR/principals.json")" -ge 1 ]] || { echo "admin query failed" >&2; exit 1; }
audit_status="$(curl -sS -o "$TEMP_DIR/audit-verify.json" -w '%{http_code}' "http://127.0.0.1:$PORT_1/v1/admin/audit-events/verify" -H "Authorization: Bearer $TOKEN_A")"
[[ "$audit_status" == 200 && "$(jq -r '.valid' "$TEMP_DIR/audit-verify.json")" == true ]] || { echo "audit-chain verification failed" >&2; exit 1; }
metrics="$(curl -sS --fail "http://127.0.0.1:$PORT_2/metrics")"
printf '%s\n' "$metrics" | grep -q 'memshare_http_requests_total'
[[ "$(printf '%s\n' "$metrics" | awk '/^memshare_http_requests_total / {print $2}')" -gt 0 ]] || { echo "process metrics did not count requests" >&2; exit 1; }

commit="$(git -C "$PROJECT_DIR" rev-parse HEAD 2>/dev/null || printf 'unknown')"
jq -n \
  --arg timestamp "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
  --arg commit "$commit" \
  --arg memory_id "$memory_id" \
  --arg handoff_id "$handoff_id" \
  '{status:"passed",instances:3,transport:"native-process",database:"shared-postgresql",commit:$commit,memory_id:$memory_id,handoff_id:$handoff_id,checks:["readiness","cross-process-idempotency","cross-process-read","tenant-isolation","cross-process-handoff","mcp-initialize","a2a-agent-card","admin-query","audit-chain","metrics"],timestamp:$timestamp}' >"$REPORT"

echo "Multi-process verification passed"
echo "Report: $REPORT"
