# Contributing

LLM-MemShare is a security-sensitive Rust beta. Start with a focused issue or proposal before changing identity, authorization, tenancy, migrations, retention, operational limits, or compatibility behavior.

Participation is governed by [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md), and maintenance decisions follow [GOVERNANCE.md](GOVERNANCE.md).

## Development

Install Rust 1.89.0 and PostgreSQL 16.

Follow [local setup](docs/GETTING_STARTED.md) for database roles and [the documentation index](docs/README.md) for API, architecture, and operations references. Documentation is maintained in English. Keep text architecture diagrams aligned with the implemented runtime and label future requirements explicitly.

Format, lint, and test every change:

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
MEMSHARE_TEST_DATABASE_URL='postgresql://admin:password@127.0.0.1:55432/memshare' \
  cargo test --all-targets --all-features
```

The test database must be isolated and the supplied role must be allowed to create and drop temporary databases and roles. Never use a production URL. With `cargo`, `curl`, `jq`, and `psql` installed, the same URL can drive `bash scripts/verify-multi-process.sh` for a native three-process check. Documentation and examples must describe only behavior that is implemented and tested.

## Change requirements

- Use named Rust modules, two-space Markdown list indentation where needed, and `cargo fmt` output.
- Add positive and negative tests for every authorization change. Tenant-facing database relations require composite tenant constraints, RLS policies, and non-owner tests.
- Released migrations are immutable. Add a new numbered migration for schema or data changes, and cover both clean installation and upgrades from supported versions.
- API changes must update `docs/API.md`, implementation status, migration/rollback notes, and the changelog.
- Protocol changes must identify the exact MCP or A2A version and distinguish the implemented method subset from conformance. Client changes require typed error and transport tests.
- CI and release changes must keep third-party actions pinned, preserve coverage and dependency-audit gates, and document artifact, checksum, SBOM, and provenance behavior.
- Do not log credentials, memory content, or production data in tests and examples.
- Keep commits focused and use Conventional Commits.

Pull requests should state the user-visible behavior, threat-model impact, schema/API compatibility, rollback boundary, and exact test evidence. A reproducible cross-tenant leak blocks merge.

## Security reports

Do not open a public issue for a suspected vulnerability. Use the repository's private vulnerability reporting flow described in [the security policy](.github/SECURITY.md).
