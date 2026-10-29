# Getting started

This guide runs the service with Cargo and an existing local PostgreSQL installation. It does not require containers or model-provider credentials. Run commands from the repository root unless a step says otherwise.

## Prerequisites

- Rust 1.89 or later, Cargo, and the toolchain in `rust-toolchain.toml`.
- PostgreSQL 16 or later running locally, with `psql` available.
- A PostgreSQL administrator allowed to create roles and a development database.
- `curl`, `openssl`, and optionally `jq` for the integration walkthrough.

```sh
git clone https://github.com/jamessfbay/LLM-MemShare.git
cd LLM-MemShare
cargo build --locked
cargo run --locked -- --help
```

Cargo produces `target/debug/llm-memshare`; a release build produces `target/release/llm-memshare`. The optional container image installs the same executable as `memshare`.

## 1. Create database identities

Open `psql` as a local PostgreSQL administrator connected to the `postgres` database. Adapt the connection flags to your installation:

```sh
psql -U postgres -d postgres
```

For a fresh installation, execute the following SQL and `psql` commands. Choose separate random passwords at the prompts. The example names must be unused on your server.

```sql
CREATE ROLE memshare_owner LOGIN NOSUPERUSER NOBYPASSRLS NOCREATEDB NOCREATEROLE;
CREATE ROLE memshare_runtime LOGIN NOSUPERUSER NOBYPASSRLS NOCREATEDB NOCREATEROLE;
\password memshare_owner
\password memshare_runtime
CREATE DATABASE memshare OWNER memshare_owner;
\connect memshare
REVOKE CREATE ON SCHEMA public FROM PUBLIC;
CREATE SCHEMA memshare AUTHORIZATION memshare_owner;
GRANT CONNECT ON DATABASE memshare TO memshare_runtime;
\quit
```

The owner runs migrations and operator provisioning. The runtime only serves HTTP requests and must not inherit or be granted the owner role. The server rejects a privileged role or a role that owns application tables.

## 2. Apply migrations

Set connection strings using the passwords chosen above. Percent-encode reserved characters in URL passwords, or choose random URL-safe development passwords. These two variable names are shell conveniences; the application reads `MEMSHARE_DATABASE_URL`.

```sh
export MEMSHARE_OWNER_DATABASE_URL='postgresql://memshare_owner:OWNER_PASSWORD@127.0.0.1:5432/memshare'
export MEMSHARE_RUNTIME_DATABASE_URL='postgresql://memshare_runtime:RUNTIME_PASSWORD@127.0.0.1:5432/memshare'

MEMSHARE_DATABASE_URL="$MEMSHARE_OWNER_DATABASE_URL" cargo run --locked -- migrate
```

Grant runtime access as the owner. Existing objects need explicit grants, and default privileges cover objects created by later migrations:

```sh
psql "$MEMSHARE_OWNER_DATABASE_URL" -v ON_ERROR_STOP=1 <<'SQL'
GRANT USAGE ON SCHEMA memshare TO memshare_runtime;
GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA memshare TO memshare_runtime;
GRANT EXECUTE ON FUNCTION memshare.authenticate_request(text) TO memshare_runtime;
ALTER DEFAULT PRIVILEGES IN SCHEMA memshare
  GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO memshare_runtime;
ALTER DEFAULT PRIVILEGES IN SCHEMA memshare
  GRANT EXECUTE ON FUNCTIONS TO memshare_runtime;
SQL
```

Migrations are embedded into the Rust executable. Use the same source revision for migration and serving. See [operations](OPERATIONS.md) before applying upgrades to an existing deployment.

## 3. Bootstrap the first tenant

Generate and retain a credential before running bootstrap. It becomes the first tenant administrator's bearer token:

```sh
export MEMSHARE_BOOTSTRAP_TOKEN="$(openssl rand -hex 32)"
MEMSHARE_DATABASE_URL="$MEMSHARE_OWNER_DATABASE_URL" \
  cargo run --locked -- bootstrap --tenant acme --workspace main --user alice
```

The command prints `tenant_id`, `workspace_id`, and `principal_id`. Retain these IDs and store the token in your secret manager. Bootstrap is accepted only while the deployment has no credentials. To create another tenant later:

```sh
MEMSHARE_DATABASE_URL="$MEMSHARE_OWNER_DATABASE_URL" \
  cargo run --locked -- provision-tenant --tenant second-team --workspace main --user bob
```

`provision-tenant` prints a newly generated administrator token once. Treat its output as secret.

## 4. Start the API

```sh
export MEMSHARE_BIND='127.0.0.1:8080'
MEMSHARE_DATABASE_URL="$MEMSHARE_RUNTIME_DATABASE_URL" cargo run --locked -- serve
```

Use a second terminal for HTTP calls:

```sh
curl --fail-with-body http://127.0.0.1:8080/health/ready
cargo run --locked -- healthcheck --address 127.0.0.1:8080
```

The readiness response is `{"status":"ready"}`. Healthcheck does not require a database URL. All other commands do. Cargo does not automatically load `.env`; export variables explicitly or load a trusted file in your shell.

## 5. Create and find a memory

In the second terminal, enter the credential and IDs retained from bootstrap:

```sh
export MEMSHARE_TOKEN='<retained bootstrap token>'
export MEMSHARE_WORKSPACE_ID='<workspace_id>'
export MEMSHARE_PRINCIPAL_ID='<principal_id>'
export MEMSHARE_URL='http://127.0.0.1:8080'

curl --fail-with-body "$MEMSHARE_URL/v1/memories" \
  -H "Authorization: Bearer $MEMSHARE_TOKEN" \
  -H 'Content-Type: application/json' \
  -H 'Idempotency-Key: first-memory' \
  -d "{\"workspace_id\":\"$MEMSHARE_WORKSPACE_ID\",\"scope\":\"private\",\"scope_id\":\"$MEMSHARE_PRINCIPAL_ID\",\"kind\":\"fact\",\"content\":\"The deployment window starts at 02:00 UTC.\",\"source\":{}}"

curl --fail-with-body "$MEMSHARE_URL/v1/memories?q=deployment&limit=10" \
  -H "Authorization: Bearer $MEMSHARE_TOKEN"
```

The create response has a server-generated `id` and `version: 1`. The list response is `{"items":[...],"next_cursor":null}`; pass a non-null cursor back unchanged to continue in descending update order. Repeating the same body and idempotency key returns the existing authorized result. A changed body under the same live key returns 409.

## 6. Inspect operational state

The bootstrap user is a tenant administrator. It can read the default limits and cursor-paginated tenant metadata:

```sh
curl --fail-with-body "$MEMSHARE_URL/v1/tenant/limits" \
  -H "Authorization: Bearer $MEMSHARE_TOKEN"

curl --fail-with-body "$MEMSHARE_URL/v1/admin/principals?limit=10" \
  -H "Authorization: Bearer $MEMSHARE_TOKEN"

curl --fail-with-body "$MEMSHARE_URL/metrics"
curl --fail-with-body "$MEMSHARE_URL/.well-known/agent-card.json"
```

New tenants default to 1,000 principals, 100,000 non-deleted memories, 100,000 queued or claimed handoffs, and 6,000 requests per credential per database-clock minute. See [operations](OPERATIONS.md) before changing them. `/metrics` and the Agent Card are public endpoints; protect or filter them at ingress if your deployment policy requires it.

## 7. Run three native API processes

The non-Docker verifier needs `cargo`, `curl`, `jq`, `psql`, and a disposable PostgreSQL administrator URL. It builds the debug binary, creates a temporary database and runtime role, starts three listeners, runs cross-process checks, then removes its processes and database objects:

```sh
MEMSHARE_TEST_DATABASE_URL='postgresql://TEST_ADMIN:TEST_PASSWORD@127.0.0.1:5432/postgres' \
  bash scripts/verify-multi-process.sh
```

Do not point this script at production. It writes a local JSON report under `.gstack/qa-reports/`.

## Next steps

Follow [integrations](INTEGRATIONS.md) to create two agents, grant directional delivery, and exchange a typed task. Read [API](API.md) for administration and memory operations, [operations](OPERATIONS.md) for configuration and recovery, and [testing](TEST_PLAN.md) for local PostgreSQL integration checks.

## Troubleshooting

| Symptom | Check |
| --- | --- |
| `MEMSHARE_DATABASE_URL is required` | Export it or assign it before the command; shell convenience URLs are not read automatically. |
| `unsafe runtime database role` | Use the separate runtime URL for `serve`; inspect ownership and role attributes. |
| Authentication function permission error | Grant runtime execution on `memshare.authenticate_request(text)` after migration. |
| Connection refused | Confirm PostgreSQL or the HTTP process is listening at the selected host/port. |
| 401 | Check bearer format, token expiry/revocation, principal status, and tenant status. |
| 429 `rate_limited` | Wait for the database-minute window to reset or have a tenant administrator inspect `requests_per_minute`. |
| 429 `quota_exceeded` | Inspect tenant limits and current principal, non-deleted memory, or active handoff counts. |
| 404 for a known UUID | Check tenant, participant, grant, membership, logical deletion, and expiry; hidden resources deliberately return 404. |
| `already_initialized` | Use `provision-tenant` for additional tenants. |
