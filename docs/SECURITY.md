# Security boundaries

LLM-MemShare is a security-sensitive beta. It has implemented isolation controls and automated tests; independent security review and production operational gates remain pending.

## Identity and tenancy

Bearer credentials are random 256-bit values. PostgreSQL stores SHA-256 digests, and generated tokens are returned once at issuance. Credentials can expire, be revoked, or be rotated. Rotation preserves the lifetime of the active credential set, so a temporary credential cannot make itself permanent. Disabling a principal revokes its active credentials and grants and disables directly owned agents. A suspended tenant cannot authenticate.

Authentication resolves the principal and tenant through a fixed, schema-qualified `SECURITY DEFINER` function with a fixed safe search path. The same database operation atomically counts the credential in a fixed minute window and updates `last_used_at` at most once per minute. Client tenant/actor headers do not establish authority. Protected handlers enter a transaction, set transaction-local tenant/principal context, recheck active identity, and apply operation authorization.

Tenant relations use composite tenant foreign keys and RLS. The runtime must be a non-owner with `NOSUPERUSER`, `NOBYPASSRLS`, `NOCREATEROLE`, and `NOCREATEDB`; startup checks these attributes and table ownership. Operators must also review inherited privileges. The migration/operator identity must not serve HTTP traffic.

RLS guards against ordinary missing tenant predicates. It does not contain a superuser, table owner, RLS-bypass role, host compromise, or runtime attacker capable of arbitrary SQL and tenant-setting manipulation. Use separate databases or deployments when isolation from a compromised shared application is required.

## Content and handoffs

Private content is owner-scoped unless explicitly granted. Workspace/project roles determine shared content access. Agents and services require explicit content grants. Tenant administration does not automatically confer private-memory access.

Handoff creation requires a directional grant and two active workspace agents. Reads and transitions recheck the acting participant's membership. Memory references are authorized when submitted and reauthorized for each reader; inaccessible IDs are omitted with counts. These counts disclose the number of omitted references to an authorized handoff participant. Handoff text itself may contain copied secrets; reference filtering cannot redact them.

Workspace membership removal also removes project memberships and revokes grants held, issued, or protected as an agent target by that principal in the workspace. Every explicit-grant authorization path rechecks active subject membership and, for agent scope, target membership. Disabling an owner disables directly owned agents, revokes their active credentials and related grants, and authentication independently requires the owning principal to remain active. Broader ownership transfer, multi-workspace offboarding policy, and hard deletion still require operator-defined workflows; the target lifecycle requirements in [tenant isolation design](TENANT_ISOLATION_DESIGN.md) go beyond the current implementation.

Stored memory, task instructions, results, and client metadata are untrusted context. MemShare does not execute that content. External agent runtimes need their own policy for tool execution and prompt construction. Provider/model labels are client metadata, not proof of provider identity or inference.

MCP and A2A requests use the same bearer authentication, tenant context, grants, quotas, and handoff use cases as native routes. They remain untrusted JSON-RPC inputs and implement only the documented method subsets. The public A2A Agent Card uses the validated `MEMSHARE_PUBLIC_BASE_URL` configuration and ignores request host and forwarded headers. MemShare does not execute model output or establish the identity of an external provider.

## Operations and remaining gates

The application does not encrypt database records itself. Use encrypted storage, TLS across trust boundaries, and secret delivery outside source control. Current audit inserts and application logs avoid memory text and tokens. Authenticated handler denials/failures and request-limit rejections are audited on a best-effort basis; unknown credentials cannot be attributed, and an audit-write failure does not replace the original response. Audit rows form a per-tenant SHA-256 chain that administrators can verify. The chain detects ordinary row or head inconsistency, but a database owner can rewrite both; external tamper-resistant collection is not implemented.

Tenant row quotas and per-credential request windows reduce accidental exhaustion but are not complete denial-of-service controls. They do not limit bytes, database cost, concurrent work, public endpoints, or traffic before the application. Public process metrics contain aggregate counters without tenant labels and reset on restart; ingress policy must decide whether to expose them.

Logical deletion preserves history and may leave content in backups. Hard deletion, retention enforcement, backup deletion replay, measured capacity, complete policy-matrix coverage, rollback/error/cancellation pool-reuse tests, operational restore exercises, full MCP/A2A conformance, and independent security review remain release gates. CI now runs a dependency audit, but its documented transitive uncompiled-`rsa` exception and the full software supply chain still require ongoing review. A reproducible authorization leak blocks release.

The [test plan](TEST_PLAN.md) identifies reproducible evidence and gaps. [Operations](OPERATIONS.md) covers role separation, suspension recovery, and migration constraints. Report vulnerabilities privately through [GitHub private reporting](https://github.com/jamessfbay/LLM-MemShare/security/advisories/new), following the [security policy](../.github/SECURITY.md).
