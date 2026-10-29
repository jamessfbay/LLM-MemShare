# Agent and model integrations

MemShare exchanges authorized memories and typed tasks between external agent runtimes. It accepts native JSON, OpenAI-style `function_call` items, Anthropic-style `tool_use` blocks, a limited MCP 2025-06-18 tool surface, and a limited A2A 0.2.6 message/task surface. The crate also exports an asynchronous Rust client. Model inference, provider credentials, and tool-loop execution belong to the orchestrator.

## Integration responsibilities

```text
External orchestrator                    MemShare
  |
  +-- register tool input schemas
  +-- call selected model/provider
  +-- validate proposed tool output
  +-- attach correct agent credential ---> authenticate + authorize
  |                                       validate request + apply transition
  |<------------------------------------- authorized JSON result
  +-- return tool result to model
  +-- continue or stop execution
```

The contract at `GET /v1/tool-contracts/handoff` is also stored in [handoff-tools-v1.json](../contracts/handoff-tools-v1.json). It describes input shapes for `memshare_create_handoff`, `memshare_claim_handoff`, and `memshare_complete_handoff`. Select the handoff UUID in the HTTP path when claiming or completing; it is not a field in those tool inputs.

Provider and model strings are untrusted metadata. The bearer token determines identity and tenant, and database policy determines authority. Treat stored task text, instructions, results, and memory as untrusted application data when constructing model prompts.

## Runnable two-agent exchange

Complete [local setup](GETTING_STARTED.md) first. These commands use Bash, `curl`, and `jq`, a tenant administrator credential, and the workspace UUID printed at bootstrap. Run the sequence in one terminal with the API running elsewhere.

```sh
export MEMSHARE_URL='http://127.0.0.1:8080'
export MEMSHARE_TOKEN='<tenant administrator token>'
export MEMSHARE_WORKSPACE_ID='<workspace UUID>'

# The administrator owns both agents in this example.
agent_a=$(curl --fail-with-body -sS "$MEMSHARE_URL/v1/agents" \
  -H "Authorization: Bearer $MEMSHARE_TOKEN" -H 'Content-Type: application/json' \
  -d "$(jq -cn --arg ws "$MEMSHARE_WORKSPACE_ID" \
    '{name:"planner",workspace_id:$ws,expires_at:null}')")
agent_b=$(curl --fail-with-body -sS "$MEMSHARE_URL/v1/agents" \
  -H "Authorization: Bearer $MEMSHARE_TOKEN" -H 'Content-Type: application/json' \
  -d "$(jq -cn --arg ws "$MEMSHARE_WORKSPACE_ID" \
    '{name:"reviewer",workspace_id:$ws,expires_at:null}')")

agent_a_id=$(jq -er '.principal_id' <<<"$agent_a")
agent_a_token=$(jq -er '.token' <<<"$agent_a")
agent_b_id=$(jq -er '.principal_id' <<<"$agent_b")
agent_b_token=$(jq -er '.token' <<<"$agent_b")
```

Keep the returned tokens secret. The target agent's owner grants Agent A the directional `handoff` action on Agent B:

```sh
curl --fail-with-body -sS "$MEMSHARE_URL/v1/grants" \
  -H "Authorization: Bearer $MEMSHARE_TOKEN" -H 'Content-Type: application/json' \
  -d "$(jq -cn --arg ws "$MEMSHARE_WORKSPACE_ID" \
    --arg subject "$agent_a_id" --arg target "$agent_b_id" \
    '{workspace_id:$ws,subject_principal_id:$subject,scope:"agent",scope_id:$target,actions:["handoff"],expires_at:null}')"
```

Create using an OpenAI-style envelope. In a live integration, the external model would supply a proposed tool call with the same shape. This example constructs it locally and requires no provider API key:

```sh
task_args=$(jq -cn --arg ws "$MEMSHARE_WORKSPACE_ID" --arg target "$agent_b_id" \
  '{workspace_id:$ws,target_agent_id:$target,
    task:{protocol_version:"1.0",summary:"Review the deployment plan",instructions:null,data:{window:"02:00 UTC"}},
    input_memory_ids:[],source:{provider:"openai",model:"example-model-a",framework:"example-runtime"},expires_at:null}')
create_call=$(jq -cn --arg args "$task_args" \
  '{type:"function_call",name:"memshare_create_handoff",arguments:$args,call_id:"example-create-1"}')

handoff=$(curl --fail-with-body -sS "$MEMSHARE_URL/v1/adapters/openai/handoffs" \
  -H "Authorization: Bearer $agent_a_token" -H 'Content-Type: application/json' \
  -H "Idempotency-Key: example-$(openssl rand -hex 8)" -d "$create_call")
handoff_id=$(jq -er '.id' <<<"$handoff")
```

The target can discover tasks through its inbox and claim the selected task with its current version:

```sh
curl --fail-with-body -sS "$MEMSHARE_URL/v1/handoffs?direction=inbox&status=queued&limit=10" \
  -H "Authorization: Bearer $agent_b_token"

handoff_version=$(jq -er '.version' <<<"$handoff")
claimed=$(curl --fail-with-body -sS "$MEMSHARE_URL/v1/handoffs/$handoff_id/claim" \
  -H "Authorization: Bearer $agent_b_token" -H 'Content-Type: application/json' \
  -d "$(jq -cn --argjson v "$handoff_version" '{version:$v,lease_seconds:300}')")
claimed_version=$(jq -er '.version' <<<"$claimed")
```

Agent B's orchestrator performs its model/tool work externally. Before the lease expires, complete using an Anthropic-style envelope:

```sh
complete_call=$(jq -cn --argjson v "$claimed_version" \
  '{type:"tool_use",name:"memshare_complete_handoff",id:"example-complete-1",
    input:{version:$v,result:{protocol_version:"1.0",summary:"Review complete",outcome:"succeeded",data:{accepted:true}},
      result_memory_ids:[],source:{provider:"anthropic",model:"example-model-b",framework:"example-runtime"}}}')

curl --fail-with-body -sS "$MEMSHARE_URL/v1/adapters/anthropic/handoffs/$handoff_id/complete" \
  -H "Authorization: Bearer $agent_b_token" -H 'Content-Type: application/json' \
  -d "$complete_call"

curl --fail-with-body -sS "$MEMSHARE_URL/v1/handoffs/$handoff_id" \
  -H "Authorization: Bearer $agent_a_token"
```

The final read has `status: "completed"`, the result summary, and authenticated actor provenance alongside informational model labels. Run the completion within the 300-second lease or reclaim using the current version after expiry. A model label does not prove that a live provider was called.

## Route mappings

| Operation | Native | OpenAI-style | Anthropic-style |
| --- | --- | --- | --- |
| Create | `/v1/handoffs` | `/v1/adapters/openai/handoffs` | `/v1/adapters/anthropic/handoffs` |
| Claim | `/v1/handoffs/{id}/claim` | `/v1/adapters/openai/handoffs/{id}/claim` | `/v1/adapters/anthropic/handoffs/{id}/claim` |
| Complete | `/v1/handoffs/{id}/complete` | `/v1/adapters/openai/handoffs/{id}/complete` | `/v1/adapters/anthropic/handoffs/{id}/complete` |

All operations above use POST. Native calls send the inner request object directly. OpenAI-style envelopes place the serialized request in `arguments`; Anthropic-style envelopes place the object in `input`. Native read, list, release, and cancel routes work with either integration.

## Rust client

`MemShareClient` uses Reqwest with Rustls and returns typed `ClientError::Transport` or `ClientError::Api { status, code, message }` errors. It covers memory create/list/get/update/delete, native handoff create/list/get/claim/complete/release/cancel, and OpenAI-style or Anthropic-style handoff creation.

```rust
use llm_memshare::{model::CreateMemoryRequest, MemShareClient};
use serde_json::json;
use uuid::Uuid;

let client = MemShareClient::new("http://127.0.0.1:8080", token)?;
let memory = client
    .create_memory(
        "rust-client-example",
        &CreateMemoryRequest {
            workspace_id: Uuid::parse_str(workspace_id)?,
            project_id: None,
            scope: "private".to_owned(),
            scope_id: Uuid::parse_str(principal_id)?,
            kind: "fact".to_owned(),
            content: "The deployment window starts at 02:00 UTC.".to_owned(),
            source: json!({"client": "rust"}),
            expires_at: None,
        },
    )
    .await?;
```

Callers can provide a configured `reqwest::Client` through `with_http_client`. The client does not currently wrap tenant administration, memory history, MCP, or A2A routes, and it does not add automatic retries. Applications must retain creation keys and mutation versions explicitly.

## MCP 2025-06-18 subset

The authenticated JSON-RPC endpoint is `POST /v1/mcp`. It implements `initialize`, `ping`, `tools/list`, and `tools/call` for `memshare_create_handoff`, `memshare_claim_handoff`, and `memshare_complete_handoff`.

```sh
curl --fail-with-body "$MEMSHARE_URL/v1/mcp" \
  -H "Authorization: Bearer $agent_a_token" \
  -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"example","version":"1"}}}'

curl --fail-with-body "$MEMSHARE_URL/v1/mcp" \
  -H "Authorization: Bearer $agent_a_token" \
  -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}'
```

`tools/call` takes `params.name` and `params.arguments`. Create arguments match the native create request. Claim and complete arguments add `handoff_id` because MCP has no HTTP path parameter. Create derives its idempotency key from `X-Request-ID`; repeat that header when retrying the same call. Requests without a JSON-RPC `id` are treated as notifications and return HTTP 204. No MCP session transport, resources, prompts, sampling, cancellation, or full conformance suite is implemented.

## A2A 0.2.6 subset

The public Agent Card is `GET /.well-known/agent-card.json`; authenticated JSON-RPC requests go to `POST /v1/a2a`. `message/send` creates a handoff from one MemShare data part, and `tasks/get` returns a handoff as an A2A task.

```sh
a2a_request=$(jq -cn --arg ws "$MEMSHARE_WORKSPACE_ID" --arg target "$agent_b_id" \
  '{jsonrpc:"2.0",id:1,method:"message/send",params:{message:{parts:[{kind:"data",data:{operation:"create_handoff",idempotency_key:"a2a-example-1",request:{workspace_id:$ws,target_agent_id:$target,task:{protocol_version:"1.0",summary:"Review the deployment plan",instructions:null,data:{}},input_memory_ids:[],source:{provider:"a2a-client",model:"external",framework:"example"},expires_at:null}}}]}}}')

curl --fail-with-body "$MEMSHARE_URL/v1/a2a" \
  -H "Authorization: Bearer $agent_a_token" \
  -H 'Content-Type: application/json' \
  -d "$a2a_request"
```

Queued, claimed, completed, and cancelled handoffs map to submitted, working, completed, and canceled A2A task states. A completed handoff result becomes a data artifact. Native routes still perform claim, release, completion, and cancellation. Streaming, push notifications, subscriptions, A2A cancellation, and full conformance are not implemented. The Agent Card advertises the configured `MEMSHARE_PUBLIC_BASE_URL`; request host and forwarded headers are not trusted.

## References, retries, and recovery

Memory IDs in `input_memory_ids` or `result_memory_ids` must be readable by the submitting agent. The server reauthorizes those IDs on every read and omits inaccessible IDs with omission counts. A handoff grant does not grant memory read/write access; issue those grants separately.

Create retries must preserve both the idempotency key and payload. For state transitions, retain the current version, handle 409 by rereading, and avoid repeating external side effects blindly. External work can run more than once after a worker crash or lease recovery; applications need their own idempotency for those effects. There is no lease renewal endpoint: bound work to the lease, release it, or recover after expiry.

The source may cancel a queued or claimed task. Cancellation changes the stored state; it does not interrupt an external model call already running. Target completion after cancellation is rejected by the version/state check.

## Compatibility and evidence

The provider mappings and protocol routes are narrow adapters over the same native handoff use cases. They are not hosted provider clients, full provider API compatibility layers, complete MCP/A2A servers, or mechanisms for transferring private reasoning or provider sessions. Any runtime capable of HTTP can use the native contract. The Rust client ships in the crate; other language SDKs remain future work.

[The main integration test](../tests/multi_tenant.rs) uses the real router, PostgreSQL, runtime RLS role, directional grants, and deterministic provider envelopes. Protocol parser/tool fixtures are unit-tested, and the [native multi-process verifier](../scripts/verify-multi-process.sh) checks MCP initialization and A2A Agent Card discovery across three API processes. This is implementation evidence for the documented subsets, not an external conformance certification. Live model quality and provider availability are outside these tests' scope.
