# Agent and model integrations

MemShare exchanges authorized memories and typed tasks between external agent runtimes. It accepts native JSON, OpenAI-style `function_call` items, and Anthropic-style `tool_use` blocks. Both envelope mappings support create, claim, and complete. Model inference, provider credentials, and tool-loop execution belong to the orchestrator.

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

## References, retries, and recovery

Memory IDs in `input_memory_ids` or `result_memory_ids` must be readable by the submitting agent. The server reauthorizes those IDs on every read and omits inaccessible IDs with omission counts. A handoff grant does not grant memory read/write access; issue those grants separately.

Create retries must preserve both the idempotency key and payload. For state transitions, retain the current version, handle 409 by rereading, and avoid repeating external side effects blindly. External work can run more than once after a worker crash or lease recovery; applications need their own idempotency for those effects. There is no lease renewal endpoint: bound work to the lease, release it, or recover after expiry.

The source may cancel a queued or claimed task. Cancellation changes the stored state; it does not interrupt an external model call already running. Target completion after cancellation is rejected by the version/state check.

## Compatibility and evidence

These are narrow model-output envelope mappings. They are not provider clients, full provider API compatibility layers, MCP/A2A implementations, or mechanisms for transferring private reasoning or provider sessions. Any runtime capable of HTTP can use the native contract. Model-provider SDKs and a published MemShare SDK remain future work.

[The integration test](../tests/multi_tenant.rs) uses the real router, PostgreSQL, runtime RLS role, directional grants, and deterministic provider envelopes. It verifies task creation, target discovery, claim/release, completion, cancellation, hidden references, and tenant/participant denial. Live model quality and provider availability are outside that test's scope.
