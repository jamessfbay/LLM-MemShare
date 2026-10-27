# Roadmap

The target is mature, self-hosted, multi-tenant memory middleware with a stable API, predictable operations, and evidence-backed isolation. The current release is an alpha core, not a finished production platform.

1. **Alpha core — in progress.** Rust service, migrations, operator tenant provisioning, user/service/agent lifecycle, workspace/project membership, credential rotation, explicit grants, memory CRUD, stable cursor pagination for leased handoffs, versions, idempotency, audit metadata, RLS, embedded OpenAPI, CI, deployment examples, and two-tenant PostgreSQL tests. Remaining alpha work: proposals, migration fixtures, broader policy and concurrency tests, and an external SDK.
2. **Operational beta.** Ownership transfer, quotas/rate limits, hard deletion and retention, backup/restore with deletion replay, metrics, runbooks, load/soak testing, and an independent security review.
3. **Retrieval and clients.** Measured keyword retrieval, optional derived vector search with an outbox and reauthorization, a Rust client plus one widely used client language, and reproducible quality/latency benchmarks.
4. **Verified adapters.** Add only integrations with pinned specifications, conformance fixtures, and tenant-isolation coverage. Keep the core usable without external models or vector services.
5. **Production candidate and 1.0.** Stable compatibility policy, upgrade/rollback evidence, signed artifacts and SBOMs, governance/support policies, security remediation, and independent deployment reproduction.

The detailed dependencies, acceptance gates, and non-goals are in the [implementation plan](IMPLEMENTATION_PLAN.md). Features move to “implemented” only when code, automated evidence, and accurate operator documentation land together.
