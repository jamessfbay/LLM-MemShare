# Multi-tenant open-source middleware implementation plan

Status: active implementation. Baseline: Rust 0.2.0 beta on 2026-10-29. Target capabilities are future work unless explicitly identified as implemented. The current beta is not independently certified as a production multi-tenant security boundary.

The [Rust feasibility review](RUST_FEASIBILITY_REVIEW.md) accepts the target architecture with staged delivery and records the remaining production release blockers.

## Product outcome

LLM-MemShare is intended to be a self-hosted, multi-tenant memory and handoff service for applications and agents. One deployment hosts multiple tenants, each controlling its users, agents, workspaces, access grants and data. Applications use a versioned HTTP API and SDK to store, retrieve, review and hand off authorized context. Stored text and model output are untrusted data.

The first acceptance journey uses two tenants. Tenant A has two users and two agents in one workspace: an authorized agent creates a handoff and another reads it. Revoking access prevents subsequent reads. Tenant B cannot discover or access A's resources through direct access, search, references, retries or exports. Audit records identify actors and operations without containing memory text or credentials.

The first release excludes cross-tenant sharing, model weight or private reasoning migration, acquisition of provider sessions, automatic external model calls and semantic search. Extraction and external providers require explicit configuration. Protocol compatibility is claimed only after conformance testing. Request-supplied tenant IDs never establish authority.

## Current baseline

The production target is a Rust service using Axum, Tokio, SQLx, Serde and tracing. Implemented code includes embedded tenant-aware migrations, operator bootstrap and tenant provisioning, tenant-scoped users and agents, workspace/project membership tables, expiring and revocable credentials, explicit grants, memory CRUD and cursor-based history, idempotent creation, server-controlled provenance, row-level security, a least-privilege runtime role check, Compose, CI, and real PostgreSQL tenant/security integration tests.

The HTTP surface now exposes tenant limits; cursor-based administrator views for principals, workspaces, grants, credentials, and audit events; a tenant audit-chain verification endpoint; stable memory cursors and prior versions; and process-local metrics. Handoffs include stable inbox/outbox cursor pagination, bounded claim leases, reclaim, release, cancellation, completion, active-membership checks, typed payloads, and symmetric provider-envelope mappings. The crate exports a partial Rust client, and authenticated MCP 2025-06-18 and A2A 0.2.6 subsets reuse the native handoff use cases. A native three-process script, LCOV generation, dependency auditing, weekly dependency updates, and checksummed release artifacts with SBOM/provenance are automated. Embedded OpenAPI remains a route inventory rather than an exhaustive schema contract.

Proposals/review, hard deletion, context bundles, external SDKs for other languages, full MCP/A2A conformance, legacy-data migration tooling, recovery proof, measured capacity, and independent security certification remain unimplemented. MemShare does not run model inference or transfer provider sessions. These gaps and the production release gates below must be resolved before a production security or maturity claim. See [README](../README.md), [API](API.md), and [security](SECURITY.md) for the exact current contract.

## Implementation snapshot

| Area | State | Executable evidence | Next gate |
| --- | --- | --- | --- |
| Rust service and configuration | Implemented | `cargo check`, Clippy, unit tests | Configuration matrix and graceful failure tests |
| Tenant schema and RLS | Implemented beta foundation | Embedded migrations and non-owner PostgreSQL tests | Full relation/action catalog audit and legacy fixture |
| Credentials, agents, and grants | Implemented beta foundation | HTTP create/rotate/grant/revoke plus lifecycle/concurrency journey | Expiration and ownership-transfer matrix |
| Memory CRUD and search | Implemented beta foundation | HTTP CRUD, stable list cursors, and authorized history | Policy matrix, byte limits, and load baseline |
| Idempotency and versions | Implemented beta foundation | Advisory-key serialization, row-locked writes, and lifecycle races | Wider deterministic retry/revocation barriers |
| Operational controls | Implemented beta foundation | Row quotas, request windows, admin queries, audit-chain verification, and process metrics | Persistent observability, measured capacity, retention, and recovery |
| Deployment | Implemented beta foundation | Non-root image, role-separated Compose, readiness, and native three-process verification | Upgrade, restore, failover, and rollback exercises |
| Handoffs and review | Implemented handoff core | Directed grant plus list/create/read/lease/release/complete and membership-removal integration journey | Proposal/review locking and expanded races |
| SDKs, OpenAPI, adapters | Partial | In-crate Rust client, route-inventory OpenAPI, provider envelopes, MCP subset, and A2A subset | Route-complete SDK and external protocol conformance |
| Supply chain and releases | Partial | Pinned Actions, initial coverage threshold and artifact, dependency audit/update workflow, tag verification tests, checksums, SBOM, and provenance | Broader platforms, signing policy, license verification, and reproducible-build evidence |

## Proposed tenant and authorization model

This is the implemented starting model, still subject to completion of the policy matrix and production release gates. The detailed [tenant isolation design](TENANT_ISOLATION_DESIGN.md) specifies the data model, policy, database roles, migration and release gates. Changes must update the policy matrix and tests.

- A deployment contains tenants. A tenant is the isolation and administrative boundary; cross-tenant access is denied. Tenant identifiers are stable and never recycled.
- A principal is a tenant-scoped user, service or agent. A person in two tenants has distinct memberships and credentials. Authentication establishes the principal and tenant; workspace selection requires a membership check.
- A workspace belongs to exactly one tenant; a project belongs to exactly one workspace. A tenant is the organization boundary in this design; a separate organization entity is deferred until a concrete need exists.
- Every resource belongs to one tenant and workspace and has an explicit owner and scope. Personal resources belong to a principal; shared resources belong to a workspace or project. Creator and owner are distinct. Agent and run resources have explicit bindings. Resources cannot silently move between tenants.
- Tenant administrators manage membership and policy but receive no implicit access to private content. Workspace and project roles govern shared resources; personal and agent content needs an explicit grant. Any break-glass mechanism requires separate policy, auditing and threat review.
- Membership removal denies newly authorized operations. Personal data remains under the tenant retention policy; no administrator-readable transfer is implied. Agent credentials tied to a removed owner are revoked or disabled. Transfer, tenant deletion and orphan handling require explicit workflows and tests.
- Grants identify subject, resource or scope, actions, issuer, expiration and revocation. Read, write, share, review, delete and administration are separately authorized; default deny applies. Delegation cannot exceed issuer authority.

Initial policy matrix, to expand into table-driven tests in M0:

| Actor | Personal memory | Workspace/project memory | Other tenant |
| --- | --- | --- | --- |
| Owner | Read, write, share and delete under tenant policy | According to membership and grants | Deny |
| Agent/service | Only explicit, unexpired grants and bound scope | Only explicit grants and applicable membership | Deny |
| Workspace member | Only explicit grants | According to role and grants | Deny |
| Tenant administrator | No implicit content access | Only explicit membership/grant for content | Deny |
| Removed or revoked principal | Deny | Deny | Deny |

M0 must resolve the complete role/action matrix, agent ownership, grant issuers, proposal approvers, list/search visibility, run credential issuance, owner removal and transfer. Unresolved cases default to denial.

## Architecture and contracts

```text
SDK / HTTP / optional protocol adapters
                  |
       authentication and tenant resolution
                  |
       centralized authorization and use cases
                  |
    PostgreSQL transactions, versions and audit
                  |
    optional asynchronous indexes and providers
```

The Rust HTTP service is the first delivery target. Axum owns transport, Tokio owns async execution, SQLx owns database access and versioned migrations, Serde owns typed contracts, and tracing owns structured telemetry. PostgreSQL is the source of truth for identity, permission and memory. All resource queries include tenant constraints, and the server authorizes every operation. PostgreSQL row-level security is defense in depth, not a substitute for application checks. Tests must prove that the runtime role cannot bypass policies. Never derive trusted tenant context from an unvalidated header.

Keep initial implementations concrete and split policy, domain operations, storage and HTTP validation as behavior is added. Introduce general adapter interfaces when a second implementation or verified integration needs them. A future index is derived and rebuildable. Its introduction requires an outbox committed with the source transaction, idempotent workers, retries and reconciliation. Candidates must be tenant-scoped and reauthorized against PostgreSQL before returning content. Revocation and deletion remain effective during indexing delays and failures.

- **Revocation:** operations whose authorization check begins after revocation commits must be denied. Already authorized in-flight operations may complete. Transaction boundaries and cache behavior must make this ordering testable.
- **Versions and review:** updates require an expected version. Review locks source and proposal in a documented order. An update committed before the source lock causes a conflict; one ordered after approval may proceed. Test both orderings and deadlock handling.
- **Idempotency:** keys are scoped to tenant, principal and operation. Store a canonical request hash, result identifier and retention deadline, not the full response. A different input under the same key returns 409. Replay checks current authorization and returns the currently readable result; deleted, expired or inaccessible results return the same documented unavailable response. Keys may be reused after the retention window.
- **Handoffs:** use a versioned schema with bounded fields and typed references. The server owns record type and verified provenance. Reject inaccessible references on creation; on retrieval/export omit inaccessible references and return a generic omission indicator. Free text can still contain secrets.
- **Deletion:** define logical and administrative hard deletion across current records, versions, idempotency metadata, derived indexes and audit metadata. Backups need retention and deletion replay after restore; do not claim immediate erasure of offline backups. Define legal holds and authority before production.
- **Context:** authorize before applying limits. Return stable pagination, ordering and truncation metadata. State which serialized fields count toward a character budget; model-token budgets require a selected tokenizer and account for uncertainty.
- **Security:** generate high-entropy credentials, store digests, reveal secrets only at issuance, and support expiration, revocation and rotation. Logs and audit metadata omit content and credentials. Do not reveal protected resource existence or execute instructions embedded in memory.

## Delivery milestones

Each milestone requires implementation, documentation and automated evidence. Dates depend on staffing and feedback; a version number alone does not establish maturity.

### M0 — Multi-tenant contracts and test foundation

- [ ] Finalize tenant hierarchy, ownership, complete authorization matrix, lifecycle scenarios and threat model. Specify administration, isolation, revocation ordering, idempotency, handoff references and deletion.
- [x] Establish CI with dedicated PostgreSQL and isolated test databases. CI fails if required database tests are skipped. Define supported Rust and PostgreSQL versions and record test counts.
- [x] Publish contribution instructions, a private vulnerability-reporting channel, version policy and minimal release checklist before a public release.
- [x] Correct existing documentation on grant expiration, audit mutability and bootstrap token strength; record known limitations.

Exit: the two-tenant journey and negative cases have executable specifications; database tests run in CI; schema and migration design are reviewed against the matrix. Pending tests cannot count as passing.

### M1 — Multi-tenant alpha core

- [x] Add versioned migrations and operator-controlled bootstrap and tenant provisioning. Legacy-data migration remains a separate explicit work package; ownership must never be inferred silently.
- [x] Implement tenant-scoped users, services and agents, workspace/project membership, grants and credential expiration, revocation and rotation. Establish runtime database roles and RLS policy tests.
- [ ] Complete typed memory and handoff lifecycle behavior, provenance, conditional updates, review locking and idempotency. Memory CRUD/history and handoff list/create/read/claim/reclaim/release/cancel/complete, stable opaque pagination, reference filtering, and directed grants are implemented; proposal/review flows and remaining concurrency cases are pending.
- [x] Deliver Axum HTTP contracts, operator CLI commands, configuration and self-hosted Compose. The core journey needs no external model or vector database.

Exit: the two-tenant journey passes against PostgreSQL, including ID guessing, search, export, grants, retry, references and revocation. Test concurrency orderings, migration from a representative prototype fixture and clean installation. Release as alpha without a production security claim.

### M2 — Security and operational beta — in progress

- [ ] Complete and test multi-workspace offboarding, ownership transfer, run-bound credentials, retention, hard deletion and recovery. Initial row quotas, per-credential fixed-minute request limits, administrator queries, and a verifiable per-tenant audit hash chain are implemented; byte/concurrency controls, retention, external audit durability, and the remaining lifecycle work are pending.
- [ ] Document TLS, least privilege, secrets, backup/restore, deletion replay, upgrades and incident response. Test connection-pool and background-operation isolation.
- [ ] Expand matrix tests across operations and scopes, including concurrent revocation, expiration, crashes and restore. Commit-time pool reuse, opposing grants, lifecycle races, database-clock leases, and credential revocation now have PostgreSQL evidence; rollback/error/cancellation and the wider matrix remain open. A reproducible authorization leak blocks release.
- [ ] Obtain independent security review and remediate findings before claiming production tenant isolation. Budget and schedule review separately; a clearly labeled beta does not require that claim.

Exit: matrix and threat-model tests pass; backup/restore and deletion replay are demonstrated; known gaps are documented. A production isolation claim requires review, remediation and the documented deployment controls.

### M3 — Retrieval and integrations

- [ ] Improve keyword search and context ranking; create a versioned labeled evaluation dataset and measure recall, latency and tenant isolation. Set thresholds after establishing a baseline.
- [ ] If justified, add optional embeddings and PostgreSQL vector indexing, model/version metadata, transactional outbox, idempotent workers, deletion propagation and reindex. Test stale indexes, retries and isolation before enabling them.
- [ ] Make extraction opt-in with validated outputs and policy-controlled review; preserve a core mode without external services.
- [ ] Complete real-client protocol integrations. MCP 2025-06-18 and A2A 0.2.6 method subsets and an in-crate Rust HTTP client are implemented through native authorization, but no full protocol conformance or route-complete SDK claim is made.

Exit: evaluation reports dataset, configuration, resource use, latency, cost and limitations; stale or unavailable indexes cannot expose unauthorized data; a real client completes the tenant-scoped journey.

### M4 — Production candidate and open-source operations

- [ ] Provide non-root images, deployable configuration, capacity guidance, persistent metrics, tracing, alerts and tested upgrade/rollback. Process-local aggregate metrics and a native three-process verification gate exist; load, soak, failure and restore exercises remain.
- [ ] Harden releases with protected branches, least-privilege build credentials, pinned dependencies, vulnerability scanning, SBOM, provenance and artifact signing. CI already pins Actions, audits dependencies, uploads LCOV, and produces checksummed Linux/macOS archives with CycloneDX SBOM and GitHub provenance; reproducible builds, broader platform coverage, signing policy, and license verification remain.
- [ ] Publish governance, support and maintenance expectations, compatibility policy, changelog, examples and migration guides. Initial governance, conduct, support, and beta compatibility policies are published; stable 1.0 deprecation/support windows, migration guides, and independent reproduction of at least two integrations remain.

Exit: operational evidence and security-review remediation are documented; integration and recovery reports are reproducible. A 1.0 release additionally requires a stable API and compatibility policy. Suitability depends on the deployment and threat model.

## Initial work packages

| Order | Deliverable | Acceptance and dependency |
| --- | --- | --- |
| 1 | Tenancy and policy contract | Complete matrix and positive/negative scenarios; resolves M0 decisions. |
| 2 | Database CI | PostgreSQL tests run without silent skips; provides evidence for subsequent changes. |
| 3 | Schema and migration | Tenant-aware schema, runtime roles and migration fixture; depends on package 1. |
| 4 | Identity and authorization | Provisioning, membership, grants, credentials and RLS defense; cross-tenant tests pass. |
| 5 | Memory and handoff correctness | Review concurrency, idempotency, provenance and reference checks; depends on 3–4. |
| 6 | End-to-end beta foundation | Rust client subset, deployment, docs, two-tenant journey, native three-process check and release controls; depends on 2–5. |

Split packages into reviewable issues and PRs with prerequisites, schema/API changes, tests, migration and rollback notes. Assign owners and estimates after the M0 design is accepted. No external repository changes are assumed.

## Verification and release evidence

Use policy unit tests, real PostgreSQL transaction and RLS tests, HTTP/SDK contract tests, negative isolation tests, concurrency and failure tests, and deployment end-to-end tests. Report passed, failed and skipped separately; use no production secrets in fixtures.

Record cold start, time to first call, p50/p95 latency and error rate with data size, concurrency, hardware and version. Retrieval reports also include Recall@k, MRR, indexing and deletion propagation delay, and external model cost. Establish baselines before setting SLOs; do not present targets as measured results. A reproducible cross-tenant leak blocks release.

Use SemVer with explicit 0.x migration notes. Release notes include breaking changes, migrations, limitations and rollback constraints. Label experimental APIs. Security claims must follow evidence and deployment conditions.

## Current release decision

The project is documented as a 0.2.0 beta foundation. Its [architecture](ARCHITECTURE.md), [local setup](GETTING_STARTED.md), [integration walkthrough](INTEGRATIONS.md), and [operations guide](OPERATIONS.md) describe the implemented service. The [test plan](TEST_PLAN.md) maps current evidence to remaining gaps.

Rollback/error/cancellation pool-reuse tests, broader concurrency orderings, complete privilege/policy-matrix checks, operational recovery evidence, measured capacity, and independent protocol conformance remain open. Production tenant-isolation claims require the release gates and independent security review described above.
