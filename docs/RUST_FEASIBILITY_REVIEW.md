# Rust implementation feasibility review

Status: accepted with staged delivery; implementation status refreshed for 0.2.0 beta on 2026-10-29. Original review date: 2026-10-26.

## Decision

The Rust implementation is feasible and appropriate for the target service. The selected stack is Axum for HTTP, Tokio for async execution, SQLx for PostgreSQL and migrations, Serde for contracts, and tracing for structured telemetry. PostgreSQL remains the source of truth and RLS remains defense in depth. This uses proven components and keeps the security boundary explicit.

The migration replaced the runtime; it was not a line-by-line translation of the prototype. The repository now contains one supported implementation in Rust. The first deliverable is the multi-tenant core: identity, tenant transactions, credentials, grants, memory operations, RLS and a two-tenant test. Search quality, automated extraction and hard deletion follow only after the isolation gates pass.

## Scope challenge

The complete product spans identity, authorization, memory lifecycle, retrieval, protocol compatibility, deployment and open-source operations. Building all of it in one rewrite would mix structural migration with several new behavioral systems. Delivery is staged without reducing the final objective:

1. Establish the Rust executable, schema, migration/runtime roles and isolation test.
2. Complete the multi-tenant alpha API and lifecycle operations.
3. Pass operational and security gates.
4. Add derived retrieval and adapters after the source-of-truth path is stable.

This ordering makes failures attributable and rollback possible. It also avoids defining plugin abstractions before a second implementation exists.

## Architecture findings and decisions

1. **[P0] Runtime database ownership would invalidate RLS evidence.** The runtime role is `NOBYPASSRLS`, is not a table owner and cannot assume migration roles. Compose separates initialization administrator, non-super migration owner, and runtime roles; the service also refuses unsafe runtime roles at startup.
2. **[P0] Tests using the default PostgreSQL superuser would always bypass RLS.** Integration tests create a non-owner role and run the service pool under that role. A direct query with the wrong tenant context must return no rows.
3. **[P1] Authentication occurs before tenant context exists.** A fixed `SECURITY DEFINER` lookup returns only credential and principal identity. The function has a fixed search path, no dynamic SQL and explicit execution grants. Tenant content remains inaccessible.
4. **[P1] Tenant provisioning needs global authority.** Bootstrap and `provision-tenant` are operator CLI commands using the migration/control-plane connection. There is no anonymous provisioning endpoint.
5. **[P1] Rust migration cannot preserve implicit Python ownership semantics.** Legacy data requires an explicit mapping and dry run before import. Ambiguous records fail closed.
6. **[P2] A single `api.rs` will become hard to review as routes grow.** Split handlers, policy and repositories after the alpha behavior is covered. Do not introduce generic traits before that split has a concrete second implementation.

## What already exists

| Existing behavior | Reuse decision |
| --- | --- |
| PostgreSQL record model and optimistic versions | Preserve semantics, replace the schema with composite tenant constraints. |
| Owner-bound agent credentials and grants | Preserve explicit grants, replace user-bound identity with tenant-scoped principals. |
| Memory CRUD and idempotency | Reimplement with tenant/principal/operation key scope and authorization on replay. |
| Handoff and proposal concepts | Preserve product behavior, implement typed Rust contracts after base memory authorization. |
| Previous client and adapters | Replaced by the in-crate asynchronous Rust client, provider-envelope mappings, and narrow MCP/A2A adapters backed by native authorization. |
| Previous tests | Removed after the Rust integration suite covered the supported behavior. |

## Request and test flow

```text
HTTP request
  |
  +-- missing/malformed token --------------------------> 401
  |
  +-- authenticate_credential(hash)
        |
        +-- revoked/expired/disabled/suspended --------> 401
        |
        +-- resolved tenant + principal
              |
              +-- BEGIN transaction
              +-- SET LOCAL validated tenant/principal
              +-- refresh active principal state
              +-- authorize action
                    |
                    +-- deny or hidden resource --------> 403/404
                    |
                    +-- execute DML under RLS
                    +-- audit metadata
                    +-- COMMIT -------------------------> typed JSON

Two-tenant integration path [implemented]
  A creates private memory
  +-- B guesses ID -------------------------------------> 404
  +-- A's agent before grant ---------------------------> 404
  +-- A grants read ------------------------------------> 200
  +-- A revokes grant ----------------------------------> 404
  +-- direct runtime SQL under B context ---------------> zero A rows
```

Required next tests after the beta foundation:

- Connection reuse after rollback, cancellation and error. Commit reuse now has PostgreSQL coverage.
- Complete credential expiry, revocation and tenant-suspension matrix. Credential revocation has a deterministic integration case.
- Expanded workspace/project role boundaries and offboarding cases; membership-removal paths now have integration coverage.
- Idempotency replay after revoke, delete and expiry, plus concurrent duplicates.
- Conditional delete conflicts and finer-grained deterministic barriers. Opposing grant insertion and grant-versus-lifecycle changes now have deterministic integration coverage; the native three-process test validates cross-process retry/read behavior but is not a full race or load test.
- Handoff grant-revocation ordering, expiration, and concurrent claim/complete barriers. Listing, cancellation, claim/release, creation, reference omission, participant visibility, version conflict, provider-envelope mappings, and cross-tenant rejection are covered by the current integration journey.
- Clean install, representative legacy upgrade and failed-migration recovery.
- Expand the runtime catalog audit to cover role assumption and every schema privilege; owner, superuser, `BYPASSRLS`, `CREATEROLE`, `CREATEDB`, and table ownership are already rejected at startup.

## Failure modes

| Code path | Production failure | Test now | Handling now | Client result |
| --- | --- | --- | --- | --- |
| Credential lookup | Database unavailable | No | Database error normalization | Stable 500; token omitted from logs |
| Tenant transaction | Pooled connection retains old context | Partial | Transaction-local settings | Commit reuse covered; rollback/error/cancellation pending |
| Grant read | Revoke races an authorized operation | Partial | Sorted locks and documented in-flight semantics | Lifecycle races covered; wider revocation snapshot matrix pending |
| Memory create | Concurrent duplicate idempotency key | No | Advisory lock and hash conflict | Existing result or 409 |
| Memory update | Stale or concurrent version | Yes | Row lock, explicit comparison and affected-row check | Three-instance race yields one 200 and one 409 |
| Migration | Partial DDL or incompatible legacy row | Clean install | Operator error | Command fails; recovery test pending |
| RLS | Service runs as owner or privileged role | Yes | Three Compose roles and startup privilege audit | Startup fails closed |
| Search | Large substring scan | No | Limit 100 | Latency can rise |

There are no intentionally silent failures. Pool-reuse/cancellation, broader concurrency orderings, and privilege-audit tests remain production release blockers.

## Performance review

The beta favors correct authorization over speculative caching. Substring search can scan accessible rows and repeated membership/grant checks may become expensive. Initial row quotas and fixed-minute request windows bound some growth and request frequency, but not bytes, database work or concurrency. Before production, collect query plans at representative sizes, add tenant-leading indexes or PostgreSQL full-text search, and set measured latency budgets. Do not cache authorization until invalidation is proven. Pool, body, query and tenant concurrency limits need load-test values.

## Parallel implementation lanes

| Lane | Modules | Dependency |
| --- | --- | --- |
| A | `migrations/`, roles and migration tests | Tenant contract |
| B | `src/auth.rs`, credential/principal lifecycle | A |
| C | `src/api.rs`, memory/grant/handoff handlers | A and B |
| D | `deploy/`, CI, observability and runbooks | Executable contract; partially parallel with C |
| E | SDK/adapters and retrieval | Stable C plus security gates |

Run A first. B and independent parts of D can proceed together. C follows A+B. E waits for security gates. Keep migration and authorization tests in one lane to avoid inconsistent policies.

## NOT in scope for the current beta foundation

- Cross-tenant sharing: changes the isolation contract and needs a separate threat model.
- Semantic/vector search: derived indexes wait until revocation and deletion are proven.
- Automatic LLM extraction: external transfer and quality evaluation are separate work.
- Full protocol servers: only documented MCP 2025-06-18 and A2A 0.2.6 subsets exist; compatibility still requires external conformance tests.
- Embedded library mode: service authorization is the first supported boundary.
- Hard erasure from offline backups: requires retention, tombstones and restore replay.
- UI/admin console: the first release is API and operator CLI only.

## Review completion

- Scope: accepted with staged delivery; the final multi-tenant objective is unchanged.
- Architecture: 6 issues identified, 5 addressed in the initial implementation and 1 planned module split.
- Code quality: typed contracts, normalized errors and centralized tenant transactions selected.
- Tests: request/isolation diagram produced; PostgreSQL tenant, quota, audit, lifecycle, clock and commit-reuse evidence plus a native three-process check exist, and the remaining test groups are tracked.
- Performance: 3 measured follow-ups identified; no authorization cache introduced.
- Critical gaps: rollback/error/cancellation pool evidence, broader concurrency orderings, complete policy/privilege coverage, recovery proof, full protocol conformance, and independent security certification remain.
- Parallelization: 5 lanes; A first, B+D partly parallel, then C and E.
