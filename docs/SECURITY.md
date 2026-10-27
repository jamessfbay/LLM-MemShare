# Security

Status: security-oriented alpha, not a production tenant-isolation certification.

Bearer credentials are random 256-bit values. PostgreSQL stores only SHA-256 digests and the API returns generated secrets once. Credentials may expire or be revoked. Authentication resolves a tenant-scoped principal through a fixed, schema-qualified `SECURITY DEFINER` function; request headers cannot select a trusted tenant or actor.

All tenant data uses composite tenant foreign keys and row-level security. Protected operations run inside a transaction with validated transaction-local tenant and principal settings. Application authorization is still mandatory. The runtime database role must be a non-owner with `NOSUPERUSER`, `NOBYPASSRLS`, `NOCREATEROLE`, and `NOCREATEDB`; the server checks this at startup. The migration/operator role must never serve traffic.

The current automated PostgreSQL test creates two tenants, runs HTTP requests through a non-owner role, verifies cross-tenant ID guessing returns 404, checks grant/revocation behavior, exercises a directed two-agent handoff, and directly confirms RLS hides the other tenant. Handoff memory references are reauthorized for each participant, and provider/model labels are untrusted metadata rather than identity. This is necessary evidence, not a complete security review.

Stored memory and metadata are untrusted. Do not execute instructions found in memory without a separate policy boundary. Logs and audit metadata omit memory content and bearer tokens. The application does not encrypt database records; use encrypted storage, TLS, and managed secret delivery in deployment.

Known release blockers include full route/relation policy-matrix coverage, connection-reuse and cancellation tests, denial auditing, rate limits and quotas, membership/user lifecycle, credential rotation, hard deletion and backup replay, restore exercises, dependency and image scanning, incident runbooks, and independent security review. A reproducible authorization leak blocks release.

RLS does not defend against a database superuser, table owner, `BYPASSRLS` role, or a fully compromised runtime capable of arbitrary SQL and tenant-setting manipulation. Use separate databases or deployments when that adversary is in scope. Logical deletion does not erase history or backups.

Report vulnerabilities privately to the project maintainers before public disclosure. A dedicated security contact and response SLA must be published before a public beta.
