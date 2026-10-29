# Testing and verification

The current suite uses Rust unit tests and one PostgreSQL HTTP integration journey. CI runs the same suite with PostgreSQL 16, then builds the release binary and verifies the source package. See [GitHub Actions](https://github.com/jamessfbay/LLM-MemShare/actions/workflows/ci.yml) for results tied to individual commits.

## Local commands

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --lib

MEMSHARE_TEST_DATABASE_URL='postgresql://TEST_ADMIN:TEST_PASSWORD@127.0.0.1:5432/postgres' \
  cargo test --all-targets --all-features

cargo build --release --locked
cargo package --locked
```

Use `cargo package --locked --allow-dirty` only when intentionally verifying uncommitted source. Packaging builds an extracted copy, checking that embedded contracts and migrations are included.

The integration-test URL must point to a dedicated local/test server whose role can create/drop databases and roles. The test creates a uniquely named database, applies embedded migrations, creates a `NOLOGIN NOBYPASSRLS` runtime role, runs requests through that role, closes pools, and drops both the database and runtime role on successful completion. Failed tests can leave those temporary objects behind. Use a disposable test server or inspect the named test objects before operator cleanup. Never use production for this test.

There are currently **7 unit tests and 1 integration test**, with many assertions inside the integration journey. Required database tests fail when their URL is missing; they do not silently skip. Local Cargo validation needs no Docker. The CI PostgreSQL service uses a container on the hosted runner; the Rust application is built and tested directly.

## Implemented evidence

| Area | Evidence in source |
| --- | --- |
| Input contracts | Token randomness, bootstrap name bounds, grant action allowlist, tool input parsing, contract JSON, handoff cursor round trip and malformed cursor rejection. |
| Runtime role | Non-owner role accepted; database owner rejected. |
| Tenant isolation | Tenant B cannot read A's memory or handoff; direct SQL under B's runtime context cannot see A's memory. |
| Memory grants | Agent denied before grant, allowed after read grant, denied after revocation. |
| Shared memory roles | Workspace member reads but cannot mutate another user's record without authority. |
| Writes and retries | Idempotent replay, different-input collision, expired-key reuse for memory and handoff creation, stale version, updates, logical deletion. |
| Request errors | Malformed JSON, UUID path, and query values use JSON errors. |
| Administration | Additional administrator and service creation, expiry-preserving credential rotation, grant revocation on workspace removal, principal disablement, owner-agent disablement and authentication, and project-membership removal when workspace membership is removed. |
| Handoffs | Self-handoff rejection, directional grant, target inbox, claim, stale-version conflict, release/reclaim, typed result, cancellation, participant and tenant denial, removed-member denial, memory-reference omission. |
| Provider envelopes | OpenAI-style create/claim and Anthropic-style create/complete within the real PostgreSQL journey; parser unit fixtures. |

Sources: [integration journey](../tests/multi_tenant.rs), [adapter tests](../src/adapters.rs), [API validation/cursor tests](../src/api.rs), and [database helpers](../src/db.rs).

Lease acquisition and release/reclaim are covered. This should not be read as evidence for every expired-lease recovery race or every provider/operation combination.

The documentation walkthrough was executed on 2026-10-27 against local PostgreSQL using a temporary fresh database and separate owner/runtime roles. Database creation, migrations, grants, bootstrap, readiness, memory create/list, and the OpenAI-style create to Anthropic-style complete example passed. The temporary database and roles were removed afterward. This validates the documented HTTP sequence without live provider calls or Docker.

## Optional multi-instance system test

```sh
./scripts/verify-multi-instance.sh
```

This script requires Docker Compose and uses [compose.multi-instance.yaml](../deploy/compose.multi-instance.yaml). It builds the runtime image, applies migrations, starts three API processes against one isolated PostgreSQL database, waits for readiness, and provisions two tenants. It tests cross-instance idempotency, a same-version concurrent update with one winner, grant/revocation visibility, tenant denial, and a two-agent exchange using different provider envelopes.

Default listeners are `127.0.0.1:28081`, `:28082`, and `:28083`; override with `MEMSHARE_INSTANCE_1_PORT`, `MEMSHARE_INSTANCE_2_PORT`, and `MEMSHARE_INSTANCE_3_PORT`. It generates test credentials and a local JSON report under `.gstack/qa-reports/`. That directory is ignored and is not part of the published documentation. Unless `MEMSHARE_KEEP_MULTI_INSTANCE_STACK=1` is set, the script tears down its stack and temporary data.

Historical local multi-instance runs are separate from the per-commit CI suite. Re-run the script when fresh deployment evidence is needed; do not treat old reports as proof for changed code. The current documentation update was verified without running Docker.

## Remaining verification gates

| Area | Required additions |
| --- | --- |
| Authentication | Full malformed/expiry/suspension matrix and credential last-use semantics. |
| Tenant isolation | Every relation/action, missing or wrong context, RLS `WITH CHECK`, pool reuse after commit/rollback/error/cancellation. |
| Authorization | Complete scope/action/actor matrix, multi-workspace subjects, grant expiry, broader owner lifecycle and inherited privileges. |
| Concurrency | Deterministic barriers for revocation, duplicate creation, expired lease recovery, cancellation, claim/complete races, deadlocks. |
| Lifecycle | Ownership transfer, last-administrator transitions, nested/delegated ownership, tenant restoration, hard deletion. |
| Operations | Existing-data migration/backfill, upgrade failure, backup/restore and deletion replay, load/soak, database failover. |
| Contracts | Complete OpenAPI input/output schemas, wider size/error/pagination checks, every provider mapping, SDK/protocol conformance. |

No live LLM is called in this suite. It verifies protocol and storage behavior, not model quality, provider availability, or private reasoning transfer. A reproducible cross-tenant leak blocks release. Report passed, failed, and skipped checks separately.
