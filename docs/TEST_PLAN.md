# Test plan

## Required commands

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
MEMSHARE_TEST_DATABASE_URL='postgresql://owner:password@127.0.0.1:55432/memshare' \
  cargo test --all-targets --all-features
cargo build --release --locked
cargo package --locked --allow-dirty
```

The PostgreSQL URL must point to an isolated test server whose role can create and drop databases and roles. The integration test creates a uniquely named database, applies embedded migrations, creates a unique `NOLOGIN NOBYPASSRLS` runtime role, runs the HTTP journey through that role, closes pools, and drops the database. Never point it at production. CI provides PostgreSQL 16 and fails if the required integration test cannot run.

## Current automated evidence

- Unit tests cover random URL-safe credential generation, bootstrap identifier validation, and the grant action allowlist.
- The database integration test verifies a safe runtime role is accepted and an owner role is rejected.
- Two tenants are created in one database. Tenant B cannot read Tenant A's memory by guessing its UUID.
- An agent cannot read private memory before a grant, can read after an explicit grant, and loses access after revocation.
- A workspace member can read a shared record but cannot update or delete another principal's record without an administrative role or explicit grant.
- Idempotent replay returns the original authorized result, key/input collisions return 409, stale versions return 409, and malformed JSON/path/query input uses stable JSON error codes.
- A direct SQL query under Tenant B's transaction context cannot see Tenant A's row, proving the test exercises RLS rather than only HTTP predicates.
- HTTP handlers run against the real Axum router, authentication function, transactions, constraints, and PostgreSQL policies.
- Tenant administration tests create a service, workspace, workspace membership, project, project membership, and rotated credential; the replaced credential immediately stops authenticating.
- Handoff tests cover target inbox discovery, source/target separation, claim leases, release and re-claim across provider mappings, typed results, and denial after workspace-membership removal.
- The deployment-level test starts three independent API instances against one PostgreSQL database. It proves cross-instance idempotent replay, collision rejection, exactly one winner for two same-version concurrent writes, grant visibility, immediate post-commit revocation visibility, and cross-tenant ID-guess denial.
- The same deployment test creates two agent identities, grants directional handoff authority, creates through an OpenAI-style function call on instance 1, claims through instance 2, completes through an Anthropic-style tool use on instance 3, and lets the source read the result from instance 1. It also proves unauthorized memory references are omitted and another tenant cannot read the handoff.

## Multi-instance verification

`scripts/verify-multi-instance.sh` is an isolated black-box system test using `deploy/compose.multi-instance.yaml`. It builds the production image, applies migrations once, waits for all three readiness endpoints, provisions two tenants, sends related operations through different instances, records a machine-readable report, and tears the stack down. It does not require or reuse a developer database.

This deployment test is optional during direct Cargo development and is not required by the no-container local workflow above. CI compiles the release artifact and runs all PostgreSQL integration tests without starting application containers.

The original infrastructure run and the expanded cross-agent run on 2026-10-26 passed. See the [multi-instance QA report](../.gstack/qa-reports/qa-report-multi-instance-2026-10-26.md) and [cross-agent/cross-LLM report](../.gstack/qa-reports/qa-report-cross-agent-llm-2026-10-26.md). This evidence covers database-mediated consistency between API instances and deterministic provider-envelope mappings; it does not cover live model quality, provider availability, load capacity, network partitions, PostgreSQL failover, or connection cancellation.

## Alpha completion matrix

| Area | Required additions |
| --- | --- |
| Authentication | Malformed, too-short, expired, revoked, disabled-principal, suspended-tenant, and credential-last-use behavior. |
| Tenant isolation | Every route and DML operation, missing/wrong context, `WITH CHECK`, list/search limits, pool reuse after commit/rollback/error/cancellation. |
| Authorization | Every scope/action/actor matrix cell, multi-workspace subjects, removed membership, issuer authority, and grant expiration. |
| Concurrency | Same-version writers are covered across instances. Add revocation before/after authorization barriers, concurrent idempotent creation, cancellation, and deterministic deadlock handling. |
| Lifecycle | Ownership transfer, last-administrator transitions, agent-owner disablement cascades, and administrative hard deletion. |
| Deployment | Clean Compose install, bootstrap, restart, migration upgrade, rollback boundary, backup/restore, and startup role rejection. |
| Contracts | Unknown fields, size limits, status/error codes, stable pagination, generated OpenAPI, and client compatibility fixtures. |
| Future features | Proposal/review locking, hard deletion and backup replay, index staleness, and broader protocol conformance. |

A reproducible cross-tenant access path is release-blocking. Test reports must distinguish passed, failed, and skipped tests; required security tests cannot be silently skipped.
