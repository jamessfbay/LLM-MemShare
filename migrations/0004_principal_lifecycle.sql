-- Bring existing rows to the lifecycle rules enforced by the application before
-- replacing the authentication boundary. Principals and credentials are not
-- FORCE-RLS tables for their owner. Grants are, so the migration temporarily
-- relaxes FORCE while retaining the tenant policy for non-owners.
UPDATE memshare.principals agent
   SET status='disabled', updated_at=now()
  FROM memshare.principals owner
 WHERE agent.tenant_id=owner.tenant_id
   AND agent.owner_principal_id=owner.id
   AND agent.kind='agent'
   AND agent.status='active'
   AND owner.status<>'active';

UPDATE memshare.credentials credential
   SET status='revoked', revoked_at=now()
  FROM memshare.principals principal
 WHERE credential.tenant_id=principal.tenant_id
   AND credential.principal_id=principal.id
   AND credential.status='active'
   AND principal.status<>'active';

ALTER TABLE memshare.grants NO FORCE ROW LEVEL SECURITY;

UPDATE memshare.grants grant_row
   SET revoked_at=now()
 WHERE grant_row.revoked_at IS NULL
   AND (
     EXISTS (
       SELECT 1 FROM memshare.principals principal
        WHERE principal.tenant_id=grant_row.tenant_id
          AND principal.id IN (grant_row.subject_principal_id,grant_row.issuer_principal_id)
          AND principal.status<>'active'
     )
     OR EXISTS (
       SELECT 1 FROM memshare.principals target
        WHERE grant_row.scope='agent'
          AND target.tenant_id=grant_row.tenant_id
          AND target.id=grant_row.scope_id
          AND target.status<>'active'
     )
     OR (
       grant_row.scope='agent'
       AND NOT EXISTS (
         SELECT 1 FROM memshare.workspace_memberships target_membership
          WHERE target_membership.tenant_id=grant_row.tenant_id
            AND target_membership.workspace_id=grant_row.workspace_id
            AND target_membership.principal_id=grant_row.scope_id
            AND target_membership.status='active'
       )
     )
     OR NOT EXISTS (
       SELECT 1 FROM memshare.workspace_memberships membership
        WHERE membership.tenant_id=grant_row.tenant_id
          AND membership.workspace_id=grant_row.workspace_id
          AND membership.principal_id=grant_row.subject_principal_id
          AND membership.status='active'
     )
     OR NOT EXISTS (
       SELECT 1 FROM memshare.workspace_memberships membership
        WHERE membership.tenant_id=grant_row.tenant_id
          AND membership.workspace_id=grant_row.workspace_id
          AND membership.principal_id=grant_row.issuer_principal_id
          AND membership.status='active'
     )
   );

ALTER TABLE memshare.grants FORCE ROW LEVEL SECURITY;

CREATE OR REPLACE FUNCTION memshare.authenticate_credential(p_token_hash text)
RETURNS TABLE(credential_id uuid,tenant_id uuid,principal_id uuid,kind text)
LANGUAGE sql
SECURITY DEFINER
STABLE
SET search_path = pg_catalog, memshare
AS $$
    SELECT c.id,c.tenant_id,c.principal_id,p.kind
      FROM memshare.credentials c
      JOIN memshare.principals p
        ON p.tenant_id=c.tenant_id AND p.id=c.principal_id
      LEFT JOIN memshare.principals owner
        ON owner.tenant_id=p.tenant_id AND owner.id=p.owner_principal_id
      JOIN memshare.tenants t ON t.id=c.tenant_id
     WHERE c.token_hash=p_token_hash
       AND c.status='active'
       AND (c.expires_at IS NULL OR c.expires_at>now())
       AND p.status='active'
       AND (p.kind<>'agent' OR owner.status='active')
       AND t.status='active'
$$;

REVOKE ALL ON FUNCTION memshare.authenticate_credential(text) FROM PUBLIC;
