# HTTP API

Status: implemented 0.2.0 beta contract. The service uses JSON over HTTP and bearer credentials. A credential resolves the tenant and principal; client-supplied tenant or actor headers never establish authority. Unknown JSON fields are rejected.

The API publishes an OpenAPI 3.1 route inventory at `GET /v1/openapi.json`. Request/response schemas in that document are not yet exhaustive; this page and the handoff tool schema provide input details. The Rust crate exports `MemShareClient` for the memory and handoff methods listed in [integrations](INTEGRATIONS.md). Only the routes in the implemented table below are available.

## Authentication and errors

Send `Authorization: Bearer <token>`. Tokens are issued during tenant bootstrap/provisioning, principal or agent creation, and credential rotation. Bootstrap uses the operator-supplied token; other issuance paths return a generated token once. Store them as secrets. Invalid, expired, revoked, disabled, or suspended-tenant identities fail authentication. A valid identity from another tenant cannot access the target resource.

Errors use a JSON object with a stable machine-readable code and message. Validation errors return 400, authentication failures 401, authorization failures 403, hidden/inaccessible resources 404, idempotency or version conflicts 409, and request or resource limits 429. Raw database errors are never returned.

Every accepted bearer credential is counted in a database-clock, per-credential minute window using the tenant's `requests_per_minute` setting. Requests that pass the authentication and request-window check receive `X-RateLimit-Limit`, `X-RateLimit-Remaining`, and `X-RateLimit-Reset` on the downstream response. Authentication and request-window rejections do not expose limit state in headers. Rate-limited requests return `rate_limited`; exhausted resource quotas return `quota_exceeded`.

## Implemented routes

| Method and path | Authorization | Behavior |
| --- | --- | --- |
| `GET /health/live` | Public | Process liveness. |
| `GET /health/ready` | Public | Database connectivity readiness. |
| `GET /metrics` | Public | Process-local Prometheus text counters for requests, response classes, authentication failures, and rate-limit rejections. |
| `GET /.well-known/agent-card.json` | Public | Returns the A2A 0.2.6 Agent Card for the implemented subset. |
| `GET /v1/tool-contracts/handoff` | Public | Returns the embedded handoff tool contract version 1.0. |
| `GET /v1/openapi.json` | Public | Returns the embedded OpenAPI 3.1 contract. |
| `POST /v1/principals` | Tenant administrator user | Creates an additional user or service, optionally creates a user as tenant administrator, and returns its credential once. |
| `PATCH /v1/principals/{principal_id}` | Tenant administrator user | Changes status and/or tenant role; self-disable and self-demotion are rejected. Disabling permanently revokes active credentials and related grants, and disables directly owned agents. An agent cannot be activated while its owner is inactive. |
| `POST /v1/principals/{principal_id}/credentials` | Principal or tenant administrator | Revokes active credentials and returns one replacement credential while preserving the active credential lifetime. |
| `PATCH /v1/tenant` | Tenant administrator user | Sets current tenant status; suspension blocks further HTTP authentication. Operator recovery is required to reactivate a suspended tenant. |
| `GET /v1/tenant/limits` | Tenant administrator | Returns principal, memory, handoff, and request limits. |
| `PATCH /v1/tenant/limits` | Tenant administrator | Updates one or more tenant limits within supported ranges. |
| `GET /v1/admin/principals` | Tenant administrator | Lists tenant principals with a stable cursor. |
| `GET /v1/admin/workspaces` | Tenant administrator | Lists tenant workspaces with a stable cursor. |
| `GET /v1/admin/grants` | Tenant administrator | Lists active and revoked tenant grants with a stable cursor. |
| `GET /v1/admin/credentials` | Tenant administrator | Lists credential metadata and `last_used_at`, never token verifiers or token values. |
| `GET /v1/admin/audit-events` | Tenant administrator | Lists operation and request audit metadata with a stable cursor. |
| `GET /v1/admin/audit-events/verify` | Tenant administrator | Recomputes the tenant audit hash chain and compares it with the stored chain head. |
| `POST /v1/workspaces` | Tenant administrator user | Creates a workspace and an administrator membership for the actor. |
| `PUT /v1/workspaces/{workspace_id}/members/{principal_id}` | Workspace administrator | Adds, restores, or changes a workspace membership. |
| `DELETE /v1/workspaces/{workspace_id}/members/{principal_id}` | Workspace administrator | Removes workspace/project memberships and revokes grants held, issued, or targeted at that principal as an agent in the workspace; administrator self-removal is rejected. |
| `POST /v1/workspaces/{workspace_id}/projects` | Workspace administrator | Creates a project and an administrator membership for the actor. |
| `PUT /v1/workspaces/{workspace_id}/projects/{project_id}/members/{principal_id}` | Project administrator | Adds, restores, or changes a project membership. |
| `DELETE /v1/workspaces/{workspace_id}/projects/{project_id}/members/{principal_id}` | Project administrator | Removes a project membership; administrator self-removal is rejected. |
| `POST /v1/agents` | Workspace administrator user | Creates a tenant-scoped agent and returns its credential once. |
| `POST /v1/grants` | User with authority over the requested scope | Creates a grant when issuer, subject, and any agent-scope target are active members of the specified workspace. |
| `DELETE /v1/grants/{grant_id}` | Grant issuer | Revokes the grant. |
| `DELETE /v1/credentials/{credential_id}` | Owner of the agent principal | Revokes the credential. |
| `POST /v1/memories` | Authorized principal | Creates a memory; requires `Idempotency-Key`. |
| `GET /v1/memories?q=&limit=&cursor=` | Authorized principal | Lists authorized, non-expired memories with descending update-order cursor pagination; limit is 1–100. |
| `GET /v1/memories/{memory_id}` | Principal with `read` authority | Returns one authorized memory or 404. |
| `GET /v1/memories/{memory_id}/versions?limit=&cursor=` | Principal with current `read` authority | Lists stored prior versions in descending version order. |
| `PATCH /v1/memories/{memory_id}` | Principal with `write` authority | Updates content using the required current `version`. |
| `DELETE /v1/memories/{memory_id}?version=` | Principal with `delete` authority | Logically deletes using the current version. |
| `POST /v1/handoffs` | Agent with a directed `handoff` grant | Creates a queued handoff; requires `Idempotency-Key`. |
| `GET /v1/handoffs?direction=&status=&cursor=&limit=` | Agent | Lists the active member's inbox, outbox, or both with stable opaque cursor pagination. |
| `GET /v1/handoffs/{handoff_id}` | Source or target agent | Returns the current handoff with references filtered by current memory authority. |
| `POST /v1/handoffs/{handoff_id}/claim` | Target agent | Acquires or reacquires a 30–3600 second lease using the current version. |
| `POST /v1/handoffs/{handoff_id}/release` | Target agent | Releases a claimed task back to the queue. |
| `POST /v1/handoffs/{handoff_id}/complete` | Target agent with an active lease | Moves `claimed` to `completed` with a result and current version. |
| `POST /v1/handoffs/{handoff_id}/cancel` | Source agent | Cancels a queued or claimed handoff. |
| `POST /v1/adapters/{openai|anthropic}/handoffs` | Same as handoff creation | Maps the provider envelope to the create contract. |
| `POST /v1/adapters/{openai|anthropic}/handoffs/{handoff_id}/claim` | Same as claim | Maps the provider envelope to the claim contract. |
| `POST /v1/adapters/{openai|anthropic}/handoffs/{handoff_id}/complete` | Same as completion | Maps the provider envelope to the completion contract. |
| `POST /v1/mcp` | Authenticated principal | JSON-RPC endpoint for the documented MCP 2025-06-18 handoff-tool subset. |
| `POST /v1/a2a` | Authenticated principal | JSON-RPC endpoint for the documented A2A 0.2.6 message/task subset. |

## Operational controls and administrator queries

New tenants start with these limits: 1,000 principals, 100,000 non-deleted memories, 100,000 queued or claimed handoffs, and 6,000 requests per credential per database-clock minute. Tenant administrators can update any subset:

```json
{
  "max_principals": 2000,
  "max_memories": 500000,
  "max_handoffs": 250000,
  "requests_per_minute": 12000
}
```

Supported ranges are 1–1,000,000 principals, 1–100,000,000 memories, 1–100,000,000 active handoffs, and 1–10,000,000 requests per minute. Principal counts include disabled principals. Memory quota counts non-deleted rows, including expired rows. Handoff quota counts only `queued` and `claimed` rows. These controls limit row counts and request frequency; they are not measured capacity guarantees or complete denial-of-service protection.

All `/v1/admin/*` list routes accept `limit` (default 50, range 1–100) and an opaque `cursor`, returning `{"items":[...],"next_cursor":null}`. Principals, workspaces, grants, and credentials order by descending `(created_at,id)`; audit events page by descending `(occurred_at,id)` and include a monotonic tenant `chain_position` used for verification. Cursor values are opaque but are not signed or bound to query filters. Audit events contain identifiers and outcomes, not memory or task content, and include `chain_position`, `previous_hash`, and `event_hash`. The verification response is `{"event_count":N,"valid":true,"head_hash":"..."}`. It detects ordinary chain breaks or content changes, but a database owner can rewrite events and the chain head; this is tamper-evident metadata, not an immutable external ledger. Authenticated 4xx/5xx request auditing is best effort; malformed or unknown credentials cannot be attributed to a tenant actor.

## Core request shapes

Create a principal with `kind` set to `user` or `service`. Services cannot hold the tenant administrator role.

```json
{
  "kind": "user",
  "name": "operations-admin",
  "tenant_role": "admin",
  "expires_at": null
}
```

Update a principal with at least one field: `{"status":"disabled"}` or `{"tenant_role":"admin"}`.

Create an agent:

```json
{
  "name": "research-reader",
  "workspace_id": "00000000-0000-0000-0000-000000000000",
  "expires_at": null
}
```

Create a grant. `workspace_id` is mandatory so the server never guesses a workspace when a principal has multiple memberships.

```json
{
  "subject_principal_id": "00000000-0000-0000-0000-000000000000",
  "workspace_id": "00000000-0000-0000-0000-000000000000",
  "scope": "private",
  "scope_id": "00000000-0000-0000-0000-000000000000",
  "actions": ["read"],
  "expires_at": null
}
```

Create a memory:

```json
{
  "workspace_id": "00000000-0000-0000-0000-000000000000",
  "project_id": null,
  "scope": "private",
  "scope_id": "00000000-0000-0000-0000-000000000000",
  "kind": "fact",
  "content": "The deployment window starts at 02:00 UTC.",
  "source": {"system": "operator"},
  "expires_at": null
}
```

Supported scopes are `private`, `workspace`, `project`, and `agent`. Supported kinds are `preference`, `fact`, `experience`, `task_state`, and `unverified`. Content is limited to 50,000 bytes and client source metadata to 16 KiB. The server wraps client source data with verified actor provenance.

Create idempotency keys are scoped to tenant, principal, and operation for 24 hours. Repeating the same live key and canonical request returns the current authorized result; using a live key with different input returns 409. An expired key can be reused and produces a new resource. Updates and deletions use optimistic versions and row locks to serialize concurrent writers.

## Handoff contract

The target agent's owner must grant the source agent the `handoff` action with `scope: "agent"` and `scope_id` equal to the target agent ID. A grant is directional; it does not allow the target to send a handoff back to the source. Both agents must be active members of the selected workspace.

Create a handoff:

```json
{
  "workspace_id": "00000000-0000-0000-0000-000000000000",
  "target_agent_id": "00000000-0000-0000-0000-000000000001",
  "task": {
    "protocol_version": "1.0",
    "summary": "Review the deployment plan",
    "instructions": null,
    "data": {}
  },
  "input_memory_ids": [],
  "source": {"provider": "provider-a", "model": "model-a", "framework": "tool-runtime"},
  "expires_at": null
}
```

Claim with `{"version":1,"lease_seconds":300}`. A successful claim increments the version and returns `lease_expires_at`. Complete before that deadline with:

```json
{
  "version": 2,
  "result": {
    "protocol_version": "1.0",
    "summary": "Review complete",
    "outcome": "succeeded",
    "data": {"accepted": true}
  },
  "result_memory_ids": [],
  "source": {"provider": "provider-b", "model": "model-b", "framework": "tool-runtime"}
}
```

Task and result envelopes require a protocol version and summary. Tasks optionally carry instructions; results require an outcome of `succeeded` or `failed`; application-specific values live under `data`. Each envelope is limited to 64 KiB and each reference list to 100 unique memory IDs. The server records authenticated actor provenance beside client-supplied provider metadata. Provider, model, and framework strings are informational and never establish identity or authority.

The supported task/result `protocol_version` is exactly `1.0`. Summaries contain 1–2,000 bytes, task instructions are at most 20,000 bytes, and `data` is a JSON object. Claim defaults to a 300-second lease and accepts 30–3,600 seconds. Lease expiry permits a new claim with the current version; it does not change the stored status automatically. Release and cancellation accept `{"version":CURRENT_VERSION}`. Completion requires an active lease. All successful transitions increment the version.

List defaults to `direction=inbox` and `limit=50` (range 1–100). Directions are `inbox`, `outbox`, and `all`; optional status is `queued`, `claimed`, `completed`, or `cancelled`. The response is `{"items":[...],"next_cursor":null}` or a continuation cursor. Pass `next_cursor` unchanged using URL encoding and keep the same filters. Ordering is descending `(created_at,id)`; every page rechecks authority.

Memory listing uses the same response envelope and defaults. It orders by descending `(updated_at,id)`, accepts a case-insensitive substring query `q` up to 500 characters, and rechecks authority on every page. Memory history lists snapshots created before successful updates and logical deletion. Its cursor is the next earlier integer version. History requires current read access to the live, non-deleted memory, so a logically deleted or otherwise inaccessible memory does not expose its history through this route.

See [the integration walkthrough](INTEGRATIONS.md) for a complete runnable exchange and [operations](OPERATIONS.md) for suspension and credential recovery.

The OpenAI mapping accepts `{"type":"function_call","name":"memshare_create_handoff","arguments":"<JSON string>","call_id":"..."}`. The Anthropic mapping accepts `{"type":"tool_use","name":"memshare_complete_handoff","input":{...},"id":"..."}`. Replace the tool name with `memshare_claim_handoff` or `memshare_complete_handoff` for the corresponding route. These are narrow input mappings, not hosted model clients or general compatibility claims.

## MCP and A2A subsets

`POST /v1/mcp` implements JSON-RPC 2.0 `initialize`, `ping`, `tools/list`, and `tools/call`. It reports MCP protocol version `2025-06-18` and exposes the three handoff tools from the embedded contract. Create uses an idempotency key derived from the HTTP request ID; clients that need stable retry identity should repeat `X-Request-ID`. Claim and complete tool arguments include `handoff_id`. Notifications without an `id` receive HTTP 204. Resources, prompts, streaming transports, session negotiation, cancellation, and other MCP methods are not implemented.

`GET /.well-known/agent-card.json` reports A2A protocol version `0.2.6`. `POST /v1/a2a` implements `message/send` for a data part shaped as `{"operation":"create_handoff","request":{...},"idempotency_key":"..."}` and `tasks/get` with `id` or `taskId`. Task states map queued/claimed/completed/cancelled to submitted/working/completed/canceled. Streaming, push notifications, task cancellation, subscriptions, and other A2A methods are not implemented. Both protocol endpoints use the same bearer identity, tenant checks, handoff grants, limits, and data path as native routes.

## Planned, not implemented

Context bundles, proposal/review workflows, exports, administrative hard deletion, additional language SDKs, full MCP/A2A conformance, hosted provider clients, and optional semantic retrieval remain milestone work. Clients must not depend on routes or protocol methods until they appear in this implemented table and have contract tests.
