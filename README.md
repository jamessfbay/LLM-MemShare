# LLM-MemShare

LLM-MemShare is a Rust service for permissioned memory and task handoffs across agents and model providers. PostgreSQL stores records and enforces tenant isolation in depth; clients authenticate with credentials issued to tenant-scoped users, services, or agents.

**Status:** Rust-only multi-tenant alpha. The core tenant, principal, workspace, project, credential, grant, memory, and agent-handoff lifecycle is implemented and tested against PostgreSQL. The project has not completed the independent review and operational gates required for a production tenant-isolation claim.

## Run locally

Requires Rust 1.89+ and PostgreSQL 16+. For local development, use an isolated PostgreSQL database and separate owner and runtime roles. The owner applies migrations and provisions tenants; the runtime role must be `NOSUPERUSER NOBYPASSRLS NOCREATEROLE NOCREATEDB` and must not own tables.

```sh
cargo run -- migrate
MEMSHARE_BOOTSTRAP_TOKEN="$(openssl rand -base64 32)" \
  cargo run -- bootstrap --tenant acme --workspace main --user alice
cargo run -- serve
```

Set `MEMSHARE_DATABASE_URL` to the owner URL for `migrate`, `bootstrap`, and `provision-tenant`, then switch it to the runtime URL before `serve`. The server refuses to start under an unsafe owner or privileged role. `.env.example` documents all settings. Compose files remain available as optional deployment examples but are not required for development.

Provision the first tenant with the owner connection. Use a random value of at least 32 bytes for the bootstrap token and retain it as the first user credential:

```sh
export MEMSHARE_BOOTSTRAP_TOKEN="$(openssl rand -base64 32)"
cargo run -- bootstrap --tenant acme --workspace main --user alice
```

For direct Cargo development, run `migrate`, `bootstrap`, and `provision-tenant` with an operator/owner URL, then run `serve` with a separately provisioned non-owner runtime URL. Never run migrations against an unreviewed application database.

Bootstrap is permitted only when there are no credentials. `provision-tenant` creates later tenants and generates their first credential. Keep tokens secret. `POST /v1/agents` returns a new agent token once; create a grant before that agent accesses a scope. The HTTP contract is documented in [`docs/API.md`](docs/API.md).

```sh
curl -X POST http://127.0.0.1:8080/v1/memories \
  -H "Authorization: Bearer $MEMSHARE_BOOTSTRAP_TOKEN" \
  -H 'Content-Type: application/json' -H 'Idempotency-Key: preference-1' \
  -d "{\"workspace_id\":\"$MEMSHARE_WORKSPACE_ID\",\"project_id\":null,\"scope\":\"private\",\"scope_id\":\"$MEMSHARE_PRINCIPAL_ID\",\"kind\":\"preference\",\"content\":\"Prefers short reports\",\"source\":{},\"expires_at\":null}"
```

Use the `workspace_id` and `principal_id` printed by bootstrap as `MEMSHARE_WORKSPACE_ID` and `MEMSHARE_PRINCIPAL_ID` in the example above.

## Cross-agent handoffs

The Rust runtime supports directed, versioned handoffs between two agents in the same tenant workspace. The target agent's owner grants the source agent the `handoff` action on the target agent scope. The source can create and cancel a task. The target discovers tasks through its inbox, claims them with a bounded lease, releases them for retry, or completes them. Expired leases can be reclaimed. Both participants must retain active workspace membership. Memory references are always reauthorized for the reader and inaccessible references are omitted with a count.

The provider-neutral contract is available from `GET /v1/tool-contracts/handoff` and in [`contracts/handoff-tools-v1.json`](contracts/handoff-tools-v1.json). OpenAI-style function calls and Anthropic-style tool-use blocks are supported symmetrically for create, claim, and complete. The service performs no model inference and never trusts provider or model names for authorization. The complete HTTP surface is published at `GET /v1/openapi.json`. See [integrations](docs/INTEGRATIONS.md) for the boundary.

Run tests against an **isolated test database**:

```sh
MEMSHARE_TEST_DATABASE_URL='postgresql://memshare:password@localhost:5432/memshare_test' cargo test
```

Unit tests run without a database. Database integration tests require an isolated database and must not be skipped in CI. See [architecture](docs/ARCHITECTURE.md), [API](docs/API.md), [security](docs/SECURITY.md), the [implementation plan](docs/IMPLEMENTATION_PLAN.md), the [Rust feasibility review](docs/RUST_FEASIBILITY_REVIEW.md), the proposed [tenant isolation design](docs/TENANT_ISOLATION_DESIGN.md), and [contributing](CONTRIBUTING.md).

## Verify horizontal behavior

The multi-instance verification starts three API processes against one isolated PostgreSQL database and proves cross-instance idempotency, optimistic-write serialization, grant and revocation propagation, tenant isolation, and a two-agent handoff using two model tool-call formats:

```sh
./scripts/verify-multi-instance.sh
```

The default listeners are `127.0.0.1:28081`, `:28082`, and `:28083`. Override them with `MEMSHARE_INSTANCE_1_PORT`, `MEMSHARE_INSTANCE_2_PORT`, and `MEMSHARE_INSTANCE_3_PORT` when necessary. The script generates test-only credentials, writes a JSON result under `.gstack/qa-reports/`, and removes its containers, network, temporary files, and database volume unless `MEMSHARE_KEEP_MULTI_INSTANCE_STACK=1` is set.
