# Compatibility Policy

LLM-MemShare uses Semantic Versioning, with additional caution for the current `0.x` beta series.

- Patch releases preserve documented HTTP behavior and database compatibility unless a security fix makes that impossible. Any exception is called out in the changelog.
- Minor `0.x` releases may contain breaking API, configuration, or migration changes. Release notes identify them and provide operator steps.
- Released migration files are immutable. Upgrades move forward through new numbered migrations; automatic downgrade migrations are not supported.
- The native HTTP API is the source contract. The embedded OpenAPI document, Rust client, provider envelopes, and protocol subsets are versioned with the server and may expose only the documented subset.
- MCP and A2A compatibility is limited to the exact protocol versions and methods listed in [INTEGRATIONS.md](INTEGRATIONS.md). Full conformance is not implied.
- Database upgrades are supported only along documented forward paths. Operators must back up and test restoration before applying migrations.

Before 1.0, the project will publish a stable deprecation window and supported-version matrix. Until then, clients should pin a compatible minor version and review every release's changelog and migration notes.
