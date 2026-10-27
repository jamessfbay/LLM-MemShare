# Documentation

LLM-MemShare is a Rust multi-tenant memory and handoff middleware service. The documents below separate the current API from future production requirements.

## Use and operate the current alpha

| Document | What it covers |
| --- | --- |
| [Project overview](../README.md) | Capabilities, system diagram, and quick start. |
| [Getting started](GETTING_STARTED.md) | Local PostgreSQL roles, Cargo workflow, first tenant and first memory. |
| [Architecture](ARCHITECTURE.md) | Component, tenant, request, memory, handoff, and deployment text diagrams. |
| [HTTP API](API.md) | Implemented routes, authorization, request shapes, and errors. |
| [Integrations](INTEGRATIONS.md) | Cross-agent/cross-LLM walkthrough and tool-envelope examples. |
| [Memory model](MEMORY_MODEL.md) | Scopes, ownership, versions, source metadata, and deletion. |
| [Operations](OPERATIONS.md) | Configuration, roles, migrations, suspension recovery, verification. |
| [Testing](TEST_PLAN.md) | Reproducible commands, existing evidence, and missing coverage. |
| [Security](SECURITY.md) | Current trust boundaries and release gates. |

## Design and future delivery

| Document | Status |
| --- | --- |
| [Roadmap](ROADMAP.md) | Delivery stages and remaining features. |
| [Implementation plan](IMPLEMENTATION_PLAN.md) | Dependencies, implemented baseline, future acceptance criteria. |
| [Tenant isolation design](TENANT_ISOLATION_DESIGN.md) | Target policy, including requirements not yet implemented. |
| [Rust feasibility review](RUST_FEASIBILITY_REVIEW.md) | Architecture decisions and remaining evidence gaps. |

The [OpenAPI document](../contracts/openapi-v1.json) inventories routes; the [handoff tool schema](../contracts/handoff-tools-v1.json) specifies tool inputs. Both are embedded in the service and available over HTTP. OpenAPI request and response schemas are not yet exhaustive; use [API.md](API.md) for request examples.

See [contributing](../CONTRIBUTING.md), [changes](../CHANGELOG.md), [license](../LICENSE), and the [private security reporting policy](../.github/SECURITY.md) for project participation.
