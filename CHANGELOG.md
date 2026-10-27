# Changelog

All notable changes are documented here. The project follows Semantic Versioning after the first tagged release; 0.x releases may contain explicitly documented breaking changes.

## Unreleased

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
