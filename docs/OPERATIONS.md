# Operations

The supported local workflow is documented in [getting started](GETTING_STARTED.md). This page describes the implemented operator controls and the remaining production requirements.

## Configuration

| Variable | Default | Behavior |
| --- | --- | --- |
| `MEMSHARE_DATABASE_URL` | Required | Owner URL for migrations/provisioning; non-owner runtime URL for `serve`. |
| `MEMSHARE_BIND` | `127.0.0.1:8080` | API listener; `serve --bind` can override it. |
| `MEMSHARE_DATABASE_MAX_CONNECTIONS` | `20` | Per-process pool size, allowed range 1–200. |
| `MEMSHARE_PUBLIC_BASE_URL` | `http://MEMSHARE_BIND` | Canonical HTTP(S) origin advertised by the public A2A Agent Card; forwarded host headers are not trusted. |
| `MEMSHARE_BOOTSTRAP_TOKEN` | Required for bootstrap only | First administrator credential, at least 32 bytes; generate randomly. |
| `RUST_LOG` | `llm_memshare=info,tower_http=info` | Structured tracing filter. |

The application does not load `.env` automatically. `MEMSHARE_POSTGRES_*` values in `.env.example` belong to the optional Compose examples and are not interpreted by the Rust configuration module. Shell variables such as `MEMSHARE_OWNER_DATABASE_URL` and `MEMSHARE_RUNTIME_DATABASE_URL` in the setup guide are conveniences, not application configuration keys.

## Database responsibilities

```text
PostgreSQL administrator
  +-- creates database, owner role, runtime role
  +-- owns database-server recovery and role administration

memshare_owner
  +-- owns application schema and objects
  +-- migrate / bootstrap / provision-tenant
  +-- grants runtime DML and authentication-function access

memshare_runtime
  +-- serve only
  +-- SELECT / INSERT / UPDATE / DELETE in memshare schema
  +-- EXECUTE fixed authenticate_request function
  +-- no table ownership or privileged role membership
```

Runtime startup checks superuser, RLS bypass, role/database creation, and table ownership. The operator must also inspect inherited privileges and avoid granting privileged role membership. Runtime DML is subject to RLS and application authorization; this role is still trusted to execute application SQL and is not a hostile-client boundary.

## Migrations and upgrades

1. Record the current Git revision, binary version, and applied migrations.
2. Take a backup and verify restoration in a separate environment before upgrades that affect persistent data.
3. Review migration SQL and stop or drain old API processes when compatibility requires it.
4. Run `migrate` with the owner connection and the new binary.
5. Ensure new tables/functions have the intended runtime grants, including `EXECUTE` on `memshare.authenticate_request(text)` for 0.2.0.
6. Start the new API binary using the runtime role and verify readiness and authenticated reads.

The current migrations create the tenant schema, handoffs, leased handoff states, lifecycle enforcement, and operational controls. Migration `0003_handoff_leases.sql` adds constraints requiring leases on claimed/completed records; deployments with pre-existing claimed/completed handoffs from `0002` require an operator-reviewed data backfill before applying it. Migration `0004_principal_lifecycle.sql` disables agents with inactive owners, revokes affected credentials and stale grants, and strengthens authentication. Migration `0005_operational_controls.sql` enables `pgcrypto`, backfills a per-tenant audit hash chain, adds tenant limits, per-credential request counters, credential-linked audit metadata, supporting indexes, and the `authenticate_request` function. Review chain backfill and limit defaults, ensure the migration identity can create the extension, and grant runtime execution on the new function before starting 0.2.0. A migration failure is not a completed upgrade. Do not edit SQLx migration history to bypass a mismatch.

There is no automatic down-migration or compatibility guarantee during beta. After accepting new writes, restoring an old database backup can lose data. Plan a forward repair or a tested reconciliation path instead. Legacy prototype imports are not implemented.

## Health and shutdown

- `GET /health/live` reports process liveness.
- `GET /health/ready` queries the database and checks core schema objects. It does not certify every migration, grant, or authorization rule.
- `GET /metrics` returns public, process-local Prometheus text counters for total requests, 2xx/4xx/5xx responses, authentication failures, and request-limit rejections. Counters reset on restart and contain no tenant labels.
- `healthcheck --address HOST:PORT` calls readiness without loading database configuration.
- `serve` handles Ctrl-C and Unix termination signals through Axum graceful shutdown.

Terminate the process you started when ending local development. API shutdown preserves PostgreSQL state. Stopping the API does not cancel claimed work; the lease can expire and the target may reclaim it with the current version.

## Multiple instances

Run the same binary against the same migrated database, with a separate listener for each process. For example, in separate terminals after setting the runtime database URL:

```sh
MEMSHARE_DATABASE_URL="$MEMSHARE_RUNTIME_DATABASE_URL" cargo run --locked -- serve --bind 127.0.0.1:8081
```

```sh
MEMSHARE_DATABASE_URL="$MEMSHARE_RUNTIME_DATABASE_URL" cargo run --locked -- serve --bind 127.0.0.1:8082
```

Account for the total connection count across all instances. A reverse proxy can distribute requests without sticky sessions. PostgreSQL stores versions, grants, idempotency records, leases, tenant limits, and request windows.

The primary three-process verifier requires no Docker:

```sh
MEMSHARE_TEST_DATABASE_URL='postgresql://TEST_ADMIN:TEST_PASSWORD@127.0.0.1:5432/postgres' \
  bash scripts/verify-multi-process.sh
```

It builds the debug binary, creates a uniquely named database and login runtime role, starts three listeners on `127.0.0.1:28081` through `:28083`, verifies shared memory state, tenant isolation, a create/claim/complete handoff across all three processes, protocol discovery, audit-chain verification, administrator access, and process metrics, writes a JSON report under `.gstack/qa-reports/`, then cleans up. Override the database host/port with `MEMSHARE_TEST_HOST` and `MEMSHARE_TEST_PORT`, and listeners with `MEMSHARE_INSTANCE_1_PORT` through `MEMSHARE_INSTANCE_3_PORT`. Use only a disposable PostgreSQL server whose supplied identity may create and drop databases and roles.

`scripts/verify-multi-instance.sh` remains an optional Docker Compose scenario with a different, handoff-heavy check set. Neither script proves database failover, network-partition behavior, or production capacity.

## Tenant limits, request windows, and administration

Each tenant has one `tenant_limits` row. Defaults and supported update ranges are:

| Control | Default | Supported range | Counted rows or requests |
| --- | ---: | ---: | --- |
| `max_principals` | 1,000 | 1–1,000,000 | All principals, including disabled principals. |
| `max_memories` | 100,000 | 1–100,000,000 | Non-deleted memories; expiry does not remove a row from the count. |
| `max_handoffs` | 100,000 | 1–100,000,000 | Handoffs in `queued` or `claimed` state. |
| `requests_per_minute` | 6,000 | 1–10,000,000 | Protected requests per credential and database-clock minute. |

Tenant administrators read or patch limits at `/v1/tenant/limits`. Resource creation locks the limit row, counts current rows, and returns 429 `quota_exceeded` at the boundary. Authentication atomically increments the per-credential minute counter before entering a tenant transaction. Successful authenticated responses include `X-RateLimit-Limit`, `X-RateLimit-Remaining`, and Unix-seconds `X-RateLimit-Reset`. A rejected window returns 429 `rate_limited`. Counter rows older than two hours are cleaned opportunistically during authentication.

Tenant administrators can page through `/v1/admin/principals`, `/workspaces`, `/grants`, `/credentials`, and `/audit-events`. Each list endpoint accepts `limit` and an opaque `cursor`. Credential output includes status, expiry, revocation, and `last_used_at`, never token hashes or values. Audit output includes actor, credential, action, outcome, target identifiers, request ID, time, monotonic tenant chain position, previous hash, and event hash, never request or memory content. `GET /v1/admin/audit-events/verify` recalculates all tenant event hashes in chain-position order and compares the last event with `audit_chain_heads`.

The chain detects ordinary row edits, deletions, reordering, and head mismatches. It is not tamper-resistant against the database owner or a privileged attacker who can rewrite both events and the head, and it is not an external append-only ledger. Export or replicate audit evidence to a separately controlled system when that threat is in scope.

These controls are a beta safety baseline. The request window is fixed, not a smoothing or distributed queue algorithm. Quotas count rows, not bytes or compute. Process metrics have no persistence, histograms, latency, database health, or tenant breakdown. Put authentication, TLS, request-size controls, and any stricter observability policy at ingress.

## Suspension and credential recovery

`PATCH /v1/tenant` with `{"status":"suspended"}` blocks subsequent authentication. A suspended tenant cannot reactivate itself over HTTP because its credentials no longer authenticate. A deployment operator must restore it using the owner connection. In an owner `psql` session, replace the UUID with the validated tenant ID:

```sql
BEGIN;
SELECT set_config('memshare.tenant_id', 'TENANT_UUID', true);
UPDATE memshare.tenants
SET status='active', updated_at=now()
WHERE id='TENANT_UUID'::uuid AND status='suspended';
COMMIT;
```

Record this operator action in your operational audit trail; direct SQL does not append an application audit event. Confirm that the update affected the intended tenant.

Disabling a principal revokes its active credentials and all grants it holds, issued, or protects as an agent-scoped target. Directly owned agents are disabled, their active credentials and related grants are revoked, and authentication also rejects an agent whose owner is inactive. Reactivating the owner does not reactivate dependent agents or restore old credentials and grants; an agent cannot be reactivated until its owner is active. A tenant administrator must deliberately reactivate each required principal, issue a replacement credential, and recreate required grants. Self-disable and administrator self-demotion are rejected.

Removing a workspace membership also removes the principal's project memberships in that workspace and revokes grants the principal holds or issued there, plus grants that target the principal as an agent. Authorization rechecks workspace membership even if stale grant data exists. This operation does not globally disable the principal or its agents because they may belong to other workspaces.

## Security and remaining operational work

Use TLS at ingress and for database traffic crossing a trust boundary. Deliver secrets outside source control. Logs and current audit inserts avoid memory text and tokens; audit events are hash-chained database metadata, not an append-only external security ledger. Authenticated handler 4xx/5xx responses and request-limit denials are recorded on a best-effort basis, but authentication failures cannot be attributed and audit-write failures only emit warnings.

Logical memory deletion leaves history and backups. Administrative hard deletion, retention enforcement, deletion replay after restore, measured capacity, persistent observability, and full backup/restore runbooks remain future work. Follow the [security](SECURITY.md), [testing](TEST_PLAN.md), and [roadmap](ROADMAP.md) documents before production adoption.

## CI, dependencies, and releases

CI separates quality and integration jobs. Quality runs formatting, Clippy, `cargo audit`, release build, and source-package verification. The audit currently ignores `RUSTSEC-2023-0071` because the affected `rsa` crate is present only through SQLx's uncompiled MySQL macro dependency; CI fails if that crate enters the compiled dependency graph. Integration enforces an initial 50% line-coverage floor, produces LCOV output, runs all targets against PostgreSQL 16, executes the native three-process verifier, and uploads the coverage artifact. Dependabot checks Cargo and pinned GitHub Actions weekly. The tag workflow reruns formatting, Clippy, the full PostgreSQL suite, the multi-process verifier, and dependency auditing before building release assets, and rejects a tag whose commit is not the current `main`.

Tags matching `v*` trigger release builds for Linux x86-64 and Apple Silicon macOS. The workflow requires the tag to match `Cargo.toml`, publishes tar archives and SHA-256 files, generates a CycloneDX JSON SBOM and aggregate checksums, requests GitHub build-provenance attestations, and creates a GitHub Release. This does not yet provide Windows binaries, container publication, artifact signing outside GitHub attestations, or reproducible-build proof.
