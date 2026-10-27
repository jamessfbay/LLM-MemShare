# Integrations

The Rust alpha exposes a provider-neutral handoff contract plus two narrow model-output mappings. It does not call external models, keep provider sessions, receive private reasoning, or persist provider-side tool history. The core service remains usable without provider credentials or network access.

## Stable tool contract

`GET /v1/tool-contracts/handoff` and `contracts/handoff-tools-v1.json` expose version 1.0 of three tools: create, claim, and complete. An orchestrator is responsible for registering these schemas with its model provider, validating model output, calling MemShare with the correct agent credential, and returning the API result to its model loop.

Provider and model strings are untrusted metadata. Authentication comes only from the bearer credential, tenancy comes only from the authenticated principal, and authorization comes only from active membership and grants. Stored tasks and results are untrusted context and are never service instructions.

## Implemented mappings

- OpenAI-style endpoints accept a `function_call` item whose `arguments` field is a JSON string for `memshare_create_handoff`, `memshare_claim_handoff`, or `memshare_complete_handoff`.
- Anthropic-style endpoints accept a `tool_use` block whose `input` field is the object for the same three operations.

Both mappings call the same internal handoff operations as the native HTTP routes. They do not bypass PostgreSQL transactions, row-level security, state versions, reference filtering, or audit events. The mapping tests use deterministic fixtures and make no claim that every API feature or SDK version is supported.

An integration must authenticate with a tenant-scoped agent credential, use server-issued UUIDs, use a unique idempotency key for creation, and treat 404 as deliberately non-enumerating. MCP, A2A, provider SDKs, model invocation, streaming tool loops, and external agent frameworks remain future work and require pinned specifications plus conformance tests.

The repository is Rust-only. There is no second runtime or provider-specific authorization path.
