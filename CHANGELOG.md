# Changelog

All notable changes are documented here. The project follows Semantic Versioning after the first tagged release; 0.x releases may contain explicitly documented breaking changes.

## Unreleased

## [0.2.0] - 2026-10-29

### Added

- Tenant administrators can inspect cursor-paginated principals, workspaces, grants, credentials, and hash-chained audit events, including credential last-use and denied-request metadata, and verify the current tenant audit chain.
- Deployments can set tenant principal, active-memory, active-handoff, and per-credential request limits; requests that pass authentication expose current request-limit headers, while limit failures return stable 429 errors.
- Memory lists now use stable cursors, and authorized readers can inspect paginated prior memory versions.
- Rust applications can use the in-crate asynchronous `MemShareClient` for memory and handoff flows with typed transport and API errors.
- Authenticated MCP 2025-06-18 `initialize`, `ping`, `tools/list`, and handoff `tools/call` methods, plus an A2A 0.2.6 Agent Card and `message/send`/`tasks/get` subset.
- A non-Docker three-process verifier exercises shared-database retries, reads, tenant isolation, a cross-process handoff lifecycle, protocol discovery, audit verification, admin queries, and metrics.
- Public process-local metrics report request classes, authentication failures, and request-limit rejections without tenant labels.

### Security

- Request authentication now updates credential last-use, enforces an atomic database-clock request window, and records authenticated denied or failed requests on a best-effort basis.
- Audit inserts serialize on a per-tenant chain head and hash event metadata so administrators can detect ordinary row modification or chain breaks; this is not an external immutable audit sink.
- Audit events receive a per-tenant chain position under the head lock, so verification remains deterministic when transaction timestamps and commit order differ.
- Lifecycle and grant operations use sorted principal locking; the PostgreSQL suite now checks opposing grants, lifecycle races, database-clock leases, credential revocation, and transaction-local pool reuse.
- CI pins third-party actions, enforces an initial line-coverage floor, runs dependency audits, and uploads coverage output; tagged releases rerun the full PostgreSQL suite before publishing, and weekly dependency updates cover Cargo and GitHub Actions.
- Tagged releases verify the Cargo version and publish checksummed Linux x86-64 and Apple Silicon archives with a CycloneDX SBOM and build-provenance attestations.

### Known limitations

- The beta has no independent production tenant-isolation certification, backup/restore proof, hard deletion, proposal workflow, or measured capacity envelope.
- MCP and A2A support is limited to the documented method subsets. MemShare does not host model inference, transfer provider sessions, or claim full protocol conformance.
- Metrics are process-local, request limiting uses fixed database-minute windows, and the Rust client does not yet cover administration, history, MCP, or A2A routes.

## [0.1.1] - 2026-10-28

### Fixed

- Credential rotation preserves the lifetime of currently valid credentials and still supports administrator recovery when no valid credential remains.
- Expired memory and handoff idempotency keys can be reused without colliding with retained history.

### For contributors

- PostgreSQL integration tests remove their generated runtime role after a successful run.

### Security

- Workspace removal revokes grants held, issued, or targeted through the removed principal and every grant check requires current workspace membership.
- Disabling a principal disables directly owned agents and revokes related credentials and grants; authentication and protected transactions independently reject agents with inactive owners.
- Grant creation locks and validates the issuer, subject, and agent target so lifecycle changes cannot leave a newly inserted stale grant.
- Migration `0004_principal_lifecycle.sql` backfills lifecycle state and strengthens the credential authentication boundary for existing installations.

## [0.1.0] - 2026-10-27

### Added

- Public documentation with text architecture diagrams, a local PostgreSQL setup guide, a two-agent cross-LLM walkthrough, and operator recovery instructions.

- Rust 1.89 service using Axum, Tokio, SQLx, Serde, and structured tracing.
- Tenant-aware PostgreSQL schema with composite foreign keys, row-level security, embedded migrations, memory history, idempotency, and audit metadata.
- Operator bootstrap and tenant provisioning commands.
- Tenant-scoped agents, credentials, grants, memory CRUD, keyword filtering, optimistic versions, and logical deletion.
- Three-role Compose deployment, non-root runtime image, health endpoints, CI, and a real two-tenant isolation test.
- Directed, versioned agent handoffs with create, read, claim, and complete operations; reference reauthorization; and server-verified actor provenance.
- A versioned provider-neutral tool contract plus OpenAI-style function-call and Anthropic-style tool-use mappings.
- Three-instance black-box verification covering two agents, two provider envelopes, concurrency, revocation propagation, and tenant isolation.
- Tenant administration APIs for users, services, delegated tenant administrators, workspaces, projects, memberships, status changes, and credential rotation.
- Handoff inbox/outbox discovery, bounded claim leases, expired-lease recovery, release, cancellation, and active-membership enforcement.
- Typed handoff task/result envelopes, symmetric OpenAI-style and Anthropic-style mappings for create/claim/complete, and an embedded OpenAPI 3.1 route contract.

### Removed

- The obsolete Python runtime, package metadata, tests, and client example. Rust is now the only supported implementation and `memshare` executable.

### Fixed

- Release artifact collection now uses Cargo's actual `llm-memshare` executable path.
- Documentation distinguishes implemented behavior, target policy, local validation, and provider-envelope fixtures.

### Security

- Runtime startup rejects owner, superuser, `BYPASSRLS`, `CREATEROLE`, and `CREATEDB` roles.
- Grant requests require an explicit workspace; the server never guesses from a subject's memberships.
- Concurrent updates and deletes lock the authorized row and verify the affected version.
- Client provenance metadata is bounded and wrapped by server-authenticated actor metadata.
- Handoff delivery requires an explicit directional grant; only the source and target can read state, and inaccessible memory references are omitted.

### Known limitations

- The current contract is an alpha and has no production tenant-isolation certification.
- Proposal/review workflows, hard deletion, external SDKs, broader protocol adapters, quotas, backup recovery, and independent security review remain pending.
