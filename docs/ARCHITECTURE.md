# Architecture

This document describes the implemented Rust alpha. The [tenant isolation design](TENANT_ISOLATION_DESIGN.md) also contains future policy requirements; the [API](API.md) is the reference for currently exposed operations.

## System components

```text
CLIENTS                          RUST SERVICE                     STORAGE

User / service  ---- native JSON ----+
                                    |
Agent orchestrator A                v
  +-- external model A       +--------------------------+
  +-- tool loop ------------>| Axum routes              |
                             | src/api.rs               |
Agent orchestrator B         |                          |
  +-- external model B       | OpenAI / Anthropic       |
  +-- tool loop ------------>| mappings: src/adapters.rs |
                             +------------+-------------+
                                          |
                             +------------v-------------+
                             | Credential authentication|
                             | src/auth.rs              |
                             +------------+-------------+
                                          |
                             +------------v-------------+
                             | Tenant transactions      |
                             | Policy + use cases       |
                             | Typed request validation |
                             | src/api.rs / src/model.rs|
                             +------------+-------------+
                                          |
                             +------------v-------------+       +----------------+
                             | SQLx pool + migrations   |------>| PostgreSQL     |
                             | src/db.rs                |       | memshare schema|
                             +--------------------------+       +----------------+

Operator CLI: migrate / bootstrap / provision-tenant
  +-- owner connection ----------------------------------------> PostgreSQL

serve: non-owner runtime connection ----------------------------> PostgreSQL
```

Tokio supplies async execution. Serde deserializes typed contracts; handlers validate field sizes and values. Tracing emits structured logs. Provider calls and execution of model-generated tools happen in client orchestrators.

## Tenant and ownership relationships

```text
Deployment
  |
  +-- Tenant A
  |     +-- Principals: users / services / agents
  |     |     +-- credentials: token digests, expiry, revocation
  |     |     +-- agent owner: same-tenant principal
  |     |
  |     +-- Workspace A1
  |     |     +-- workspace memberships: principal + role + status
  |     |     +-- Project A1.1
  |     |     |     +-- project memberships
  |     |     +-- memories: private / workspace / project / agent
  |     |     +-- grants: subject + action + scope + expiry
  |     |     +-- handoffs: source agent -> target agent
  |     |
  |     +-- memory versions / idempotency records / audit events
  |
  +-- Tenant B: separate identities and tenant-scoped records
```

Composite keys and foreign keys carry tenant IDs through relationships. PostgreSQL row-level security (RLS) adds a tenant predicate to ordinary access. Application policy decides the principal's actions within that tenant. Workspace/project roles do not imply access to another principal's private content. Agents and services need explicit content grants.

The runtime role is a shared application identity. A tenant setting is not a cryptographic identity: RLS does not contain an attacker who can execute arbitrary SQL as the runtime or compromise the database owner. Stronger isolation requires separate databases or deployments.

## Protected request flow

```text
HTTP request
  |
  +-- validate JSON / path / query / bearer header
  |
  +-- SHA-256(token) -> fixed authenticate_credential function
  |     +-- inactive / expired / revoked identity -> 401
  |     +-- returns tenant ID + principal ID + kind
  |
  +-- BEGIN on one pooled connection
  +-- set_config(tenant, principal, transaction_local=true)
  +-- recheck active tenant + principal
  +-- application authorization for operation and resource
  |     +-- denied or hidden -> 403 / 404 + rollback
  |
  +-- read/write with tenant predicates and RLS
  +-- prepare authorized response; filter memory references
  +-- append operation audit metadata
  +-- COMMIT -> JSON response

Any transaction error -> rollback; transaction-local settings end
```

Extractor execution order varies by handler. Protected operations share authentication and tenant transactions; public health and contract routes do not enter a tenant transaction. Authorization uses current database state without a process-local permission cache. An operation authorized before revocation commits may finish; later authorization statements observe committed revocation. Pool reuse and cancellation still need the expanded tests listed in the [test plan](TEST_PLAN.md).

The fixed authentication function is the pre-tenant lookup path. It returns identity fields only, uses a fixed safe search path, and requires an explicit execution grant. Migrations and provisioning use the separate owner connection. `serve` rejects an owner, superuser, `BYPASSRLS`, `CREATEROLE`, or `CREATEDB` role.

## Memory path

```text
create -> validate scope + content + provenance
       -> serialize tenant/principal/operation/idempotency key
       -> check request hash -> insert memory + key + audit -> commit

update/delete -> find authorized row -> lock row
              -> compare expected version
              -> preserve previous content in memory_versions
              -> change content or deleted_at; increment version -> commit

read/list -> filter tenant + authority + deletion + expiry
          -> apply ordering/limit -> return permitted rows
```

Idempotent replay reauthorizes the result. Updates and deletes reject stale versions with 409. Deletion is logical; history and backups can retain content. Search is a bounded substring query, with no vector database or background index.

## Cross-LLM task exchange

```text
Owner of Agent B -- grant(handoff, subject=A, target=B) --> MemShare

Agent A / model A          MemShare / PostgreSQL        Agent B / model B
       |                           |                          |
       |-- create task ---------->|                          |
       |   native / function_call |-- store queued v1         |
       |                           |<-- inbox query ----------|
       |                           |--- authorized task ------>|
       |                           |<-- claim v1, lease -------|
       |                           |--- claimed v2 ------------>|
       |                           |                          |-- external work
       |                           |<-- complete v2 -----------|
       |                           |   native / tool_use       |
       |                           |-- store completed v3      |
       |-- read ------------------>|                          |
       |<-- result ---------------|                          |
```

Either provider mapping can create, claim, or complete. Both call native use cases and preserve the same authorization, transactions, versions, and reference filtering. Provider/model labels are informational; verified actor provenance comes from bearer authentication.

Task/result JSON contains protocol version `1.0`, a summary, and application data. Creation requires both agents to be active members of the selected workspace and a directional handoff grant. Reads and transitions recheck the acting participant's workspace membership. Every referenced memory ID is reauthorized for its reader; inaccessible IDs are omitted with omission counts. Task text can still contain copied secrets, so clients must choose payloads carefully.

## Handoff state machine

```text
queued -- target claim, bounded lease --> claimed
  ^                                        |
  +----------- target release -------------+
                                           |
claimed -- expired lease + target claim --> claimed (new lease)
claimed -- target complete within lease --> completed
queued  -- source cancel ----------------> cancelled
claimed -- source cancel ----------------> cancelled
```

Each successful transition increments the version. Completed and cancelled records are terminal. An expired lease leaves the record in `claimed`; it becomes claimable again rather than automatically changing to `queued`. Handoff `expires_at` is separate from lease expiry: an expired handoff is hidden from normal read/list/transition operations. There is no scheduler or external queue.

## Multiple API instances

```text
                  Clients / optional reverse proxy
                    /             |             \
                   v              v              v
              API process 1  API process 2  API process 3
                   \              |             /
                    +-------------+------------+
                                  |
                                  v
                         Shared PostgreSQL
                permissions / idempotency / row locks
                     versions / leases / task state
```

The API tier is stateless. Related requests may reach different processes; no sticky session is required. PostgreSQL is the single consistency dependency. The optional multi-instance script exercises memory writes, revocation visibility, tenant denial, and deterministic cross-provider handoffs across three processes. Database failover, capacity, load balancer behavior, and network partitions need separate operational evidence.

## Module map and extension points

| Location | Responsibility |
| --- | --- |
| `src/main.rs`, `src/config.rs` | CLI, environment configuration, startup, graceful shutdown. |
| `src/auth.rs` | Bearer authentication and request principal context. |
| `src/api.rs` | HTTP extraction, policy checks, queries, state transitions, auditing. |
| `src/model.rs`, `src/error.rs` | Typed payloads/responses and stable error envelope. |
| `src/adapters.rs`, `contracts/` | Provider-envelope mappings and public contracts. |
| `src/db.rs`, `migrations/` | Connection pool, role validation, provisioning, versioned schema. |
| `tests/multi_tenant.rs` | Real PostgreSQL HTTP and RLS integration journey. |

Future derived indexes must remain rebuildable and reauthorize candidates against PostgreSQL. Proposed extraction, approval, retention, and deletion workflows are described in the [implementation plan](IMPLEMENTATION_PLAN.md); they are not part of the implemented runtime.
