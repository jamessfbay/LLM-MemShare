# Operations

The supported local workflow is documented in [getting started](GETTING_STARTED.md). This page describes the implemented operator controls and the remaining production requirements.

## Configuration

| Variable | Default | Behavior |
| --- | --- | --- |
| `MEMSHARE_DATABASE_URL` | Required | Owner URL for migrations/provisioning; non-owner runtime URL for `serve`. |
| `MEMSHARE_BIND` | `127.0.0.1:8080` | API listener; `serve --bind` can override it. |
| `MEMSHARE_DATABASE_MAX_CONNECTIONS` | `20` | Per-process pool size, allowed range 1–200. |
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
  +-- EXECUTE fixed authenticate_credential function
  +-- no table ownership or privileged role membership
```

Runtime startup checks superuser, RLS bypass, role/database creation, and table ownership. The operator must also inspect inherited privileges and avoid granting privileged role membership. Runtime DML is subject to RLS and application authorization; this role is still trusted to execute application SQL and is not a hostile-client boundary.

## Migrations and upgrades

1. Record the current Git revision, binary version, and applied migrations.
2. Take a backup and verify restoration in a separate environment before upgrades that affect persistent data.
3. Review migration SQL and stop or drain old API processes when compatibility requires it.
4. Run `migrate` with the owner connection and the new binary.
5. Ensure new tables/functions have the intended runtime grants.
6. Start the new API binary using the runtime role and verify readiness and authenticated reads.

The current migrations create the tenant schema, handoffs, and leased handoff states. Migration `0003_handoff_leases.sql` adds constraints requiring leases on claimed/completed records; deployments with pre-existing claimed/completed handoffs from `0002` require an operator-reviewed data backfill before applying it. A migration failure is not a completed upgrade. Do not edit SQLx migration history to bypass a mismatch.

There is no automatic down-migration or compatibility guarantee during alpha. After accepting new writes, restoring an old database backup can lose data. Plan a forward repair or a tested reconciliation path instead. Legacy prototype imports are not implemented.

## Health and shutdown

- `GET /health/live` reports process liveness.
- `GET /health/ready` queries the database and checks core schema objects. It does not certify every migration, grant, or authorization rule.
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

Account for the total connection count across all instances. A reverse proxy can distribute requests without sticky sessions. PostgreSQL stores versions, grants, idempotency records, and leases. The optional `scripts/verify-multi-instance.sh` automates a separate three-instance scenario with Compose and does require Docker; ordinary local development does not.

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

Disabling a principal revokes its active credentials. Reactivating that principal does not restore the old tokens; a tenant administrator must issue a replacement through credential rotation. Self-disable and administrator self-demotion are rejected. Disabling an agent's owner does not yet cascade to that owner's agents; revoke or disable those agents explicitly.

## Security and remaining operational work

Use TLS at ingress and for database traffic crossing a trust boundary. Deliver secrets outside source control. Logs and current audit inserts avoid memory text and tokens; audit events are database metadata, not an append-only external security ledger. Denied operations are not fully audited.

Logical memory deletion leaves history and backups. Administrative hard deletion, retention enforcement, deletion replay after restore, rate limits, quotas, measured capacity, and full backup/restore runbooks remain future work. Follow the [security](SECURITY.md), [testing](TEST_PLAN.md), and [roadmap](ROADMAP.md) documents before production adoption.
