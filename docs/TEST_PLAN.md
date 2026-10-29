# Testing and verification

The current suite uses Rust unit tests, two PostgreSQL HTTP/security integration journeys, and a native three-process system check. CI runs coverage against PostgreSQL 16, then performs separate formatting, lint, dependency-audit, release-build, source-package, and multi-process gates. See [GitHub Actions](https://github.com/jamessfbay/LLM-MemShare/actions/workflows/ci.yml) for results tied to individual commits.

## Local commands

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --lib

MEMSHARE_TEST_DATABASE_URL='postgresql://TEST_ADMIN:TEST_PASSWORD@127.0.0.1:5432/postgres' \
  cargo test --all-targets --all-features

cargo build --release --locked
cargo package --locked

# Requires cargo-llvm-cov 0.9.1 when reproducing the CI coverage artifact.
MEMSHARE_TEST_DATABASE_URL='postgresql://TEST_ADMIN:TEST_PASSWORD@127.0.0.1:5432/postgres' \
  cargo llvm-cov --all-targets --all-features --fail-under-lines 50 --lcov --output-path lcov.info

MEMSHARE_TEST_DATABASE_URL='postgresql://TEST_ADMIN:TEST_PASSWORD@127.0.0.1:5432/postgres' \
  bash scripts/verify-multi-process.sh
```

Use `cargo package --locked --allow-dirty` only when intentionally verifying uncommitted source. Packaging builds an extracted copy, checking that embedded contracts and migrations are included.

The integration-test URL must point to a dedicated local/test server whose role can create/drop databases and roles. Each integration target creates a uniquely named database, applies embedded migrations, creates a `NOLOGIN NOBYPASSRLS` runtime role, runs requests through that role, closes pools, and drops its temporary objects on successful completion. Failed tests can leave those objects behind. The multi-process script creates a login runtime role because it starts standalone processes. Use a disposable test server or inspect the named test objects before operator cleanup. Never use production for these tests.

There are currently **12 unit tests and 2 PostgreSQL integration tests**, with many assertions inside both integration journeys. Required database tests fail when their URL is missing; they do not silently skip. Local Cargo and native multi-process validation need no Docker. The CI PostgreSQL service uses a container on the hosted runner; the Rust application is built and tested directly.

## Implemented evidence

| Area | Evidence in source |
| --- | --- |
| Input contracts | Token randomness, bootstrap name bounds, grant action allowlist, tool input parsing, current OpenAPI route inventory, contract JSON, handoff cursor round trip and malformed cursor rejection. |
| Runtime role | Non-owner role accepted; database owner rejected. |
| Tenant isolation | Tenant B cannot read A's memory or handoff; direct SQL under B's runtime context cannot see A's memory. |
| Memory grants | Agent denied before grant, allowed after read grant, denied after revocation. |
| Shared memory roles | Workspace member reads but cannot mutate another user's record without authority. |
| Writes and retries | Idempotent replay, different-input collision, expired-key reuse for memory and handoff creation, memory cursor/history, stale version, updates, logical deletion. |
| Request errors | Malformed JSON, UUID path, and query values use JSON errors. |
| Administration | Additional administrator and service creation, expiry-preserving credential rotation, grant revocation on workspace removal, principal disablement, owner-agent disablement and authentication, and project-membership removal when workspace membership is removed. |
| Handoffs | Self-handoff rejection, directional grant, target inbox, claim, stale-version conflict, release/reclaim, typed result, cancellation, participant and tenant denial, removed-member denial, memory-reference omission. |
| Provider envelopes | OpenAI-style create/claim and Anthropic-style create/complete within the real PostgreSQL journey; parser unit fixtures. |
| Operational controls | Tenant limit read/update, principal quota rejection, per-credential request rejection, `last_used_at`, process metrics, and cursor-based admin queries. |
| Audit | Successful and best-effort denied-request metadata, request/credential identity, hash linkage, full-chain verification, and deterministic chain positions when transaction timestamp order differs from append order. |
| Concurrency and clocks | Opposing grant insertion, grant versus agent lifecycle changes, database-clock lease expiry/reclaim, credential revocation, and transaction-local context reuse after commit. |
| Rust client | Base URL normalization, authenticated request path/header/body checks, typed success and 429 decoding, unreadable error fallback, transport failure, and compilation across all targets. |
| Protocol subsets | MCP tool projection and authenticated create/claim/complete calls, malformed-parameter error mapping, A2A create/get calls, initialization, and Agent Card discovery through native authorization paths. |

Sources: [tenant integration journey](../tests/multi_tenant.rs), [security/concurrency journey](../tests/security_concurrency.rs), [native multi-process verifier](../scripts/verify-multi-process.sh), [adapter tests](../src/adapters.rs), [protocol tests](../src/protocols.rs), [API tests](../src/api.rs), [client tests](../src/client.rs), and [database helpers](../src/db.rs).

Lease acquisition and release/reclaim are covered. This should not be read as evidence for every expired-lease recovery race or every provider/operation combination.

The documentation walkthrough was executed on 2026-10-27 against local PostgreSQL using a temporary fresh database and separate owner/runtime roles. Database creation, migrations, grants, bootstrap, readiness, memory create/list, and the OpenAI-style create to Anthropic-style complete example passed. The temporary database and roles were removed afterward. This validates the documented HTTP sequence without live provider calls or Docker.

## Native multi-process system test

```sh
MEMSHARE_TEST_DATABASE_URL='postgresql://TEST_ADMIN:TEST_PASSWORD@127.0.0.1:5432/postgres' \
  bash scripts/verify-multi-process.sh
```

The script builds the debug binary and uses `psql` to create a disposable database and runtime login role. It starts three native API processes on ports `28081`–`28083` by default and checks readiness, cross-process creation replay, cross-process memory read, cross-tenant denial, create/claim/complete handoff transitions across all three processes, MCP initialization, A2A Agent Card discovery, a tenant-admin query, audit-chain verification, and non-zero process metrics. It writes a JSON report under `.gstack/qa-reports/` and removes its processes, database, role, and temporary directory on exit. CI runs this script after the coverage suite.

This check does not exercise quota races, the Rust client, load, database failover, or full protocol conformance. Those claims require their dedicated tests or future evidence.

## Optional Compose multi-instance system test

```sh
./scripts/verify-multi-instance.sh
```

This script requires Docker Compose and uses [compose.multi-instance.yaml](../deploy/compose.multi-instance.yaml). It builds the runtime image, applies migrations, starts three API processes against one isolated PostgreSQL database, waits for readiness, and provisions two tenants. It tests cross-instance idempotency, a same-version concurrent update with one winner, grant/revocation visibility, tenant denial, and a two-agent exchange using different provider envelopes.

Default listeners are `127.0.0.1:28081`, `:28082`, and `:28083`; override with `MEMSHARE_INSTANCE_1_PORT`, `MEMSHARE_INSTANCE_2_PORT`, and `MEMSHARE_INSTANCE_3_PORT`. It generates test credentials and a local JSON report under `.gstack/qa-reports/`. That directory is ignored and is not part of the published documentation. Unless `MEMSHARE_KEEP_MULTI_INSTANCE_STACK=1` is set, the script tears down its stack and temporary data.

Historical local Compose runs are separate from the per-commit CI suite. Re-run the script when fresh container evidence is needed; do not treat old reports as proof for changed code. The native multi-process script, not this Compose scenario, is the current CI gate.

## Coverage, dependency, and release gates

CI installs `cargo-llvm-cov` 0.9.1, produces LCOV for all targets/features, and uploads `lcov.info`. No percentage threshold is enforced yet. The quality job installs `cargo-audit` 0.22.2 and currently ignores `RUSTSEC-2023-0071` only because `rsa` is locked through an uncompiled SQLx MySQL macro path; the compiled PostgreSQL graph must continue to show no reverse dependency. Third-party Actions are pinned by commit, and Dependabot checks Cargo and Actions weekly.

Tagged release automation verifies the tag against the Cargo version, builds Linux x86-64 and Apple Silicon macOS binaries, produces per-archive and aggregate SHA-256 checksums, generates a CycloneDX 1.5 JSON SBOM, requests build-provenance attestations, and publishes a GitHub Release. These controls do not prove reproducible builds or replace independent artifact and dependency review.

## Remaining verification gates

| Area | Required additions |
| --- | --- |
| Authentication | Full malformed/expiry/suspension matrix and credential last-use semantics. |
| Tenant isolation | Every relation/action, missing or wrong context, RLS `WITH CHECK`, pool reuse after rollback/error/cancellation. Commit reuse is covered. |
| Authorization | Complete scope/action/actor matrix, multi-workspace subjects, grant expiry, broader owner lifecycle and inherited privileges. |
| Concurrency | Deterministic barriers for authorization/revocation snapshots, duplicate creation, cancellation, claim/complete races, and bounded deadlock retry. Opposing grant and lifecycle races plus database-clock lease recovery are covered. |
| Lifecycle | Ownership transfer, last-administrator transitions, nested/delegated ownership, tenant restoration, hard deletion. |
| Operations | Existing-data migration/backfill, upgrade failure, backup/restore and deletion replay, byte/concurrency quotas, persistent metrics, load/soak, database failover. |
| Contracts | Complete OpenAPI input/output schemas, wider size/error/pagination checks, Rust client route expansion, every provider mapping, and external MCP/A2A conformance. |

No live LLM is called in this suite. It verifies documented protocol subsets and storage behavior, not model quality, provider availability, full MCP/A2A conformance, or private reasoning transfer. A reproducible cross-tenant leak blocks release. Report passed, failed, and skipped checks separately.
