# LLM-MemShare

[![CI](https://github.com/jamessfbay/LLM-MemShare/actions/workflows/ci.yml/badge.svg)](https://github.com/jamessfbay/LLM-MemShare/actions/workflows/ci.yml)
[![License: Apache-2.0](https://img.shields.io/badge/License-Apache--2.0-blue.svg)](LICENSE)

Permissioned memory and task handoffs for agents, across model providers.

LLM-MemShare is open-source middleware written in Rust. Applications use one HTTP service to store shared context, control access, and pass versioned tasks between agents. A deployment can host multiple tenants; bearer credentials identify each tenant and principal, while PostgreSQL stores authorization and task state.

**Status: 0.1.1 alpha.** The implemented API includes tenant administration, credentials, grants, memory CRUD, and leased agent handoffs. Production security review, recovery exercises, and remaining release gates are tracked in the [roadmap](docs/ROADMAP.md).

## Architecture at a glance

```text
Application / agent orchestrators
  |
  +-- Agent A: model/provider A -- function_call --+
  +-- Agent B: model/provider B -- tool_use -------+-- Authenticated HTTP
  +-- Any HTTP client ----------- native JSON ----+          |
                                                           v
  +------------------------------------------------------------+
  | LLM-MemShare: stateless Rust API processes                   |
  |                                                            |
  | Native routes / provider envelope mappings                  |
  |                         |                                  |
  | Credential lookup -> tenant + principal -> authorization   |
  |                         |                                  |
  | Administration | memories + grants | leased handoffs       |
  |                         |                                  |
  | SQLx transactions, versions, idempotency, audit metadata    |
  +-------------------------+----------------------------------+
                            |
                            v
  +------------------------------------------------------------+
  | PostgreSQL: source of truth                                |
  | Tenants / principals / memberships / credentials / grants  |
  | Memories / history / handoffs / idempotency / audit events |
  | Composite tenant constraints + row-level security          |
  +------------------------------------------------------------+
```

External orchestrators run model calls and tool loops. MemShare stores the authorized context and task state they exchange. See the [detailed architecture](docs/ARCHITECTURE.md) for tenant relationships, request processing, state transitions, and horizontal deployment.

## What works today

| Capability | Implemented behavior |
| --- | --- |
| Multiple tenants | Operator provisioning, tenant-scoped identities, database row-level security. |
| Administration | Users, services, agents, tenant roles, workspaces, projects, memberships, principal disablement, tenant suspension. |
| Credentials | Random bearer tokens, digest storage, expiration, revocation, rotation. |
| Permissioned memory | Private/workspace/project/agent scopes, explicit grants, CRUD, keyword filtering, history, logical deletion. |
| Safe retries | Idempotency keys for creation; versions and row locks for mutations. |
| Agent handoffs | Inbox/outbox with cursors, create, claim, lease recovery, release, complete, cancel. |
| Cross-LLM exchange | Native JSON plus OpenAI-style and Anthropic-style envelopes for create, claim, and complete. |
| Operations | Embedded migrations, runtime-role checks, readiness, structured logs, CI and source-package verification. |

## Start locally

Requires Rust **1.89+** and PostgreSQL **16+**. The normal development path uses Cargo and a local PostgreSQL service. Follow the [complete setup guide](docs/GETTING_STARTED.md) to create the owner/runtime roles and database first.

```sh
git clone https://github.com/jamessfbay/LLM-MemShare.git
cd LLM-MemShare
cargo build --locked

# Set these to the connections created in the setup guide.
export MEMSHARE_OWNER_DATABASE_URL='postgresql://memshare_owner:OWNER_PASSWORD@127.0.0.1:5432/memshare'
export MEMSHARE_RUNTIME_DATABASE_URL='postgresql://memshare_runtime:RUNTIME_PASSWORD@127.0.0.1:5432/memshare'

MEMSHARE_DATABASE_URL="$MEMSHARE_OWNER_DATABASE_URL" cargo run --locked -- migrate

# Retain this secret: it becomes the first user's bearer credential.
export MEMSHARE_BOOTSTRAP_TOKEN="$(openssl rand -hex 32)"
MEMSHARE_DATABASE_URL="$MEMSHARE_OWNER_DATABASE_URL" \
  cargo run --locked -- bootstrap --tenant acme --workspace main --user alice

# Apply the runtime grants from the setup guide before serving.
MEMSHARE_DATABASE_URL="$MEMSHARE_RUNTIME_DATABASE_URL" cargo run --locked -- serve
```

Bootstrap prints `workspace_id` and `principal_id`. In another terminal, set those IDs and the retained bootstrap credential, then store a memory:

```sh
export MEMSHARE_TOKEN='<retained bootstrap token>'
export MEMSHARE_WORKSPACE_ID='<workspace_id>'
export MEMSHARE_PRINCIPAL_ID='<principal_id>'

curl --fail-with-body http://127.0.0.1:8080/v1/memories \
  -H "Authorization: Bearer $MEMSHARE_TOKEN" \
  -H 'Content-Type: application/json' \
  -H 'Idempotency-Key: first-preference' \
  -d "{\"workspace_id\":\"$MEMSHARE_WORKSPACE_ID\",\"scope\":\"private\",\"scope_id\":\"$MEMSHARE_PRINCIPAL_ID\",\"kind\":\"preference\",\"content\":\"Prefers short reports\",\"source\":{}}"
```

The Cargo executable is `target/debug/llm-memshare` or `target/release/llm-memshare`. The optional container image installs it as `memshare`. The server defaults to `127.0.0.1:8080`; environment files are not loaded automatically.

## Cross-agent and cross-LLM handoffs

```text
Target owner grants Agent A permission to hand off to Agent B
                              |
Agent A + model A -- create --> queued task (version 1)
                              |
Agent B + model B -- inbox ---> discover task
                 -- claim ---> claimed task (version 2, bounded lease)
                 -- work ----> model/tool execution in B's orchestrator
                 -- complete > completed task (version 3, typed result)
                              |
Agent A ---------- read ------> consume result
```

Both agents belong to the same tenant workspace. Reading a handoff does not grant access to its referenced memories: each reference is reauthorized for the reader. OpenAI-style `function_call` and Anthropic-style `tool_use` mappings support the same create/claim/complete operations. These mappings are tested with deterministic fixtures; live provider inference and provider sessions remain the orchestrator's responsibility.

Use the [integration walkthrough](docs/INTEGRATIONS.md) for runnable HTTP examples and the [tool schemas](contracts/handoff-tools-v1.json) for model tool registration.

## API and verification

Public endpoints: `GET /health/live`, `GET /health/ready`, `GET /v1/openapi.json`, and `GET /v1/tool-contracts/handoff`. All resource routes use bearer authentication. The embedded OpenAPI document is a route inventory; [API documentation](docs/API.md) and the handoff tool schema provide request details.

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --lib

# Use a dedicated local test server with database/role creation privileges.
MEMSHARE_TEST_DATABASE_URL='postgresql://TEST_ADMIN:TEST_PASSWORD@127.0.0.1:5432/postgres' \
  cargo test --all-targets --all-features
```

Integration tests create a temporary database and run requests through a non-owner runtime role. [Testing documentation](docs/TEST_PLAN.md) describes coverage and remaining gaps. The optional `scripts/verify-multi-instance.sh` system test uses Docker Compose; it is separate from the local Cargo workflow.

## Documentation

Start with the [documentation index](docs/README.md), [setup guide](docs/GETTING_STARTED.md), [architecture](docs/ARCHITECTURE.md), [API](docs/API.md), [integrations](docs/INTEGRATIONS.md), and [operations](docs/OPERATIONS.md).

The current release does not include model inference, private reasoning/session transfer, MCP/A2A conformance, semantic/vector retrieval, proposal approval, administrative hard deletion, or a published external SDK. See the [roadmap](docs/ROADMAP.md) and [implementation plan](docs/IMPLEMENTATION_PLAN.md).

Contributions follow [CONTRIBUTING.md](CONTRIBUTING.md). Report vulnerabilities through [private vulnerability reporting](https://github.com/jamessfbay/LLM-MemShare/security/advisories/new), as described in the [security policy](.github/SECURITY.md). Licensed under [Apache-2.0](LICENSE).
