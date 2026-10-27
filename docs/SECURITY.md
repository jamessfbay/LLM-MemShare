# Security boundaries

LLM-MemShare is a security-sensitive alpha. It has implemented isolation controls and automated tests; independent security review and production operational gates remain pending.

## Identity and tenancy

Bearer credentials are random 256-bit values. PostgreSQL stores SHA-256 digests, and generated tokens are returned once at issuance. Credentials can expire, be revoked, or be rotated. Disabling a principal revokes its active credentials. A suspended tenant cannot authenticate.

Authentication resolves the principal and tenant through a fixed, schema-qualified `SECURITY DEFINER` function with a fixed safe search path. Client tenant/actor headers do not establish authority. Protected handlers enter a transaction, set transaction-local tenant/principal context, recheck active identity, and apply operation authorization.

Tenant relations use composite tenant foreign keys and RLS. The runtime must be a non-owner with `NOSUPERUSER`, `NOBYPASSRLS`, `NOCREATEROLE`, and `NOCREATEDB`; startup checks these attributes and table ownership. Operators must also review inherited privileges. The migration/operator identity must not serve HTTP traffic.

RLS guards against ordinary missing tenant predicates. It does not contain a superuser, table owner, RLS-bypass role, host compromise, or runtime attacker capable of arbitrary SQL and tenant-setting manipulation. Use separate databases or deployments when isolation from a compromised shared application is required.

## Content and handoffs

Private content is owner-scoped unless explicitly granted. Workspace/project roles determine shared content access. Agents and services require explicit content grants. Tenant administration does not automatically confer private-memory access.

Handoff creation requires a directional grant and two active workspace agents. Reads and transitions recheck the acting participant's membership. Memory references are authorized when submitted and reauthorized for each reader; inaccessible IDs are omitted with counts. These counts disclose the number of omitted references to an authorized handoff participant. Handoff text itself may contain copied secrets; reference filtering cannot redact them.

Workspace membership removal also removes project memberships in that workspace. It does not revoke every explicit memory grant or disable dependent agents. Disabling an owner does not yet cascade to owned agents. Operators must explicitly revoke grants and disable affected agents according to their offboarding policy. The target lifecycle requirements in [tenant isolation design](TENANT_ISOLATION_DESIGN.md) go beyond the current implementation.

Stored memory, task instructions, results, and client metadata are untrusted context. MemShare does not execute that content. External agent runtimes need their own policy for tool execution and prompt construction. Provider/model labels are client metadata, not proof of provider identity or inference.

## Operations and remaining gates

The application does not encrypt database records itself. Use encrypted storage, TLS across trust boundaries, and secret delivery outside source control. Current audit inserts and application logs avoid memory text and tokens. Audit rows are ordinary database metadata; full denial auditing and external tamper-resistant collection are not implemented.

Logical deletion preserves history and may leave content in backups. Hard deletion, retention enforcement, backup deletion replay, quotas/rate limits, complete policy-matrix coverage, cancellation/pool-reuse tests, operational restore exercises, dependency scanning, and independent security review remain release gates. A reproducible authorization leak blocks release.

The [test plan](TEST_PLAN.md) identifies reproducible evidence and gaps. [Operations](OPERATIONS.md) covers role separation, suspension recovery, and migration constraints. Report vulnerabilities privately through [GitHub private reporting](https://github.com/jamessfbay/LLM-MemShare/security/advisories/new), following the [security policy](../.github/SECURITY.md).
