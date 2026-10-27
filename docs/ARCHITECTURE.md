# Architecture

LLM-MemShare is a single Rust binary backed by PostgreSQL. Axum handles HTTP, Tokio provides the async runtime, SQLx owns connection pooling, transactions, and embedded migrations, Serde validates contracts, and tracing emits structured logs.

```text
HTTP client
    |
bearer credential lookup (fixed SECURITY DEFINER function)
    |
authenticated tenant + principal
    |
transaction-local tenant context + application authorization
    |
PostgreSQL constraints, RLS, versions, idempotency, audit metadata
```

Every protected request authenticates a credential digest, begins one transaction, sets `memshare.tenant_id` and `memshare.principal_id` with transaction-local `set_config`, verifies the principal and tenant are active, authorizes the requested action, performs the mutation or query, appends audit metadata, and commits. Connection-pool reuse cannot retain transaction-local context.

Tenant isolation is layered. Composite foreign keys prevent cross-tenant relationships. Every tenant-owned table has row-level security. Resource queries also include tenant and authorization predicates. The `serve` command refuses to start if its database role owns application tables or has superuser, `BYPASSRLS`, `CREATEROLE`, or `CREATEDB`. Migrations and operator provisioning use a separate owner connection.

The current memory path supports private, workspace, project, and agent scopes. Users receive role-based access to workspace/project records through active membership; private data stays owner-only unless explicitly granted. Agents have no ambient content authority and need explicit action grants. Memory creation is idempotent, updates/deletes are versioned and row-locked, logical deletes preserve history, and list filtering occurs before the limit.

Handoffs are directed records between two active agents in one workspace. A target agent owner explicitly grants a source agent the `handoff` action on that target. The source creates a queued task, the target atomically claims and completes it, and optimistic versions serialize state transitions. Only the two participants can read the record. Referenced memories are checked when written and reauthorized for every reader; inaccessible identifiers are omitted. Model/provider metadata is stored as untrusted client data beside server-verified actor provenance.

PostgreSQL is the source of truth. Future search indexes must be derived, rebuildable, tenant-scoped, and reauthorized against PostgreSQL before returning content. Revocation and deletion must remain effective during index delay or failure.

The API tier is stateless and can run multiple instances behind a load balancer. Credentials, grants, idempotency records, versions, and authorization state live in PostgreSQL, so correctness does not depend on process-local state or sticky sessions. A three-instance black-box test verifies this property for the implemented memory and grant flows. PostgreSQL remains the single consistency and availability dependency; database high availability, load balancer behavior, capacity limits, and network-partition recovery are deployment responsibilities that still require production-environment exercises.

## Trust boundaries and limitations

Stored text and client metadata are untrusted data. They are never instructions to the service. SHA-256 protects random high-entropy token lookup but is not suitable for low-entropy user passwords. The fixed authentication function is the only pre-tenant lookup path and must remain schema-qualified with a safe search path.

RLS protects against ordinary application mistakes; it does not contain a database superuser, table owner, `BYPASSRLS` role, or arbitrary SQL execution that can change the custom tenant setting. Deployments needing containment from a fully compromised shared runtime should use stronger database or deployment isolation.

Logical deletion is not secure erasure. Historical versions and backups may retain content. Handoff listing/cancellation, proposals, stable pagination, lifecycle administration, quotas, hard deletion, backup deletion replay, external identity providers, SDKs, and general protocol adapters are planned rather than implemented.
