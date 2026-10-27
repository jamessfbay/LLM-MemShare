CREATE SCHEMA IF NOT EXISTS memshare;

CREATE FUNCTION memshare.current_tenant_id() RETURNS uuid
LANGUAGE sql STABLE PARALLEL SAFE
RETURN NULLIF(current_setting('memshare.tenant_id', true), '')::uuid;

CREATE FUNCTION memshare.current_principal_id() RETURNS uuid
LANGUAGE sql STABLE PARALLEL SAFE
RETURN NULLIF(current_setting('memshare.principal_id', true), '')::uuid;

CREATE TABLE memshare.tenants (
    id uuid PRIMARY KEY,
    slug text NOT NULL UNIQUE CHECK (slug ~ '^[A-Za-z0-9._-]{1,100}$'),
    status text NOT NULL CHECK (status IN ('active','suspended','deleting')),
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE memshare.principals (
    tenant_id uuid NOT NULL REFERENCES memshare.tenants(id),
    id uuid NOT NULL,
    kind text NOT NULL CHECK (kind IN ('user','service','agent')),
    name text NOT NULL CHECK (char_length(name) BETWEEN 1 AND 200),
    status text NOT NULL CHECK (status IN ('active','disabled')),
    tenant_role text NOT NULL DEFAULT 'member' CHECK (tenant_role IN ('admin','member')),
    owner_principal_id uuid,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id,id),
    FOREIGN KEY (tenant_id,owner_principal_id)
        REFERENCES memshare.principals(tenant_id,id),
    CHECK ((kind='agent' AND owner_principal_id IS NOT NULL) OR kind<>'agent')
);

CREATE TABLE memshare.credentials (
    id uuid PRIMARY KEY,
    tenant_id uuid NOT NULL,
    principal_id uuid NOT NULL,
    token_hash text NOT NULL UNIQUE CHECK (char_length(token_hash)=64),
    status text NOT NULL CHECK (status IN ('active','revoked')),
    expires_at timestamptz,
    revoked_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now(),
    last_used_at timestamptz,
    FOREIGN KEY (tenant_id,principal_id)
        REFERENCES memshare.principals(tenant_id,id),
    CHECK ((status='revoked') = (revoked_at IS NOT NULL))
);

CREATE TABLE memshare.workspaces (
    tenant_id uuid NOT NULL REFERENCES memshare.tenants(id),
    id uuid NOT NULL,
    slug text NOT NULL CHECK (slug ~ '^[A-Za-z0-9._-]{1,100}$'),
    status text NOT NULL CHECK (status IN ('active','archived')),
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id,id),
    UNIQUE (tenant_id,slug)
);

CREATE TABLE memshare.workspace_memberships (
    tenant_id uuid NOT NULL,
    workspace_id uuid NOT NULL,
    principal_id uuid NOT NULL,
    role text NOT NULL CHECK (role IN ('admin','member')),
    status text NOT NULL CHECK (status IN ('active','removed')),
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id,workspace_id,principal_id),
    FOREIGN KEY (tenant_id,workspace_id)
        REFERENCES memshare.workspaces(tenant_id,id),
    FOREIGN KEY (tenant_id,principal_id)
        REFERENCES memshare.principals(tenant_id,id)
);

CREATE TABLE memshare.projects (
    tenant_id uuid NOT NULL,
    workspace_id uuid NOT NULL,
    id uuid NOT NULL,
    slug text NOT NULL CHECK (slug ~ '^[A-Za-z0-9._-]{1,100}$'),
    status text NOT NULL CHECK (status IN ('active','archived')),
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id,workspace_id,id),
    UNIQUE (tenant_id,workspace_id,slug),
    FOREIGN KEY (tenant_id,workspace_id)
        REFERENCES memshare.workspaces(tenant_id,id)
);

CREATE TABLE memshare.project_memberships (
    tenant_id uuid NOT NULL,
    workspace_id uuid NOT NULL,
    project_id uuid NOT NULL,
    principal_id uuid NOT NULL,
    role text NOT NULL CHECK (role IN ('admin','member')),
    status text NOT NULL CHECK (status IN ('active','removed')),
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id,workspace_id,project_id,principal_id),
    FOREIGN KEY (tenant_id,workspace_id,project_id)
        REFERENCES memshare.projects(tenant_id,workspace_id,id),
    FOREIGN KEY (tenant_id,principal_id)
        REFERENCES memshare.principals(tenant_id,id)
);

CREATE TABLE memshare.grants (
    tenant_id uuid NOT NULL,
    id uuid NOT NULL,
    issuer_principal_id uuid NOT NULL,
    subject_principal_id uuid NOT NULL,
    workspace_id uuid NOT NULL,
    scope text NOT NULL CHECK (scope IN ('private','workspace','project','agent')),
    scope_id uuid NOT NULL,
    actions text[] NOT NULL CHECK (cardinality(actions)>0 AND actions <@ ARRAY['read','write','delete','review']::text[]),
    expires_at timestamptz,
    revoked_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id,id),
    FOREIGN KEY (tenant_id,issuer_principal_id)
        REFERENCES memshare.principals(tenant_id,id),
    FOREIGN KEY (tenant_id,subject_principal_id)
        REFERENCES memshare.principals(tenant_id,id),
    FOREIGN KEY (tenant_id,workspace_id)
        REFERENCES memshare.workspaces(tenant_id,id)
);

CREATE TABLE memshare.memories (
    tenant_id uuid NOT NULL,
    id uuid NOT NULL,
    workspace_id uuid NOT NULL,
    project_id uuid,
    scope text NOT NULL CHECK (scope IN ('private','workspace','project','agent')),
    scope_id uuid NOT NULL,
    owner_principal_id uuid,
    creator_principal_id uuid NOT NULL,
    kind text NOT NULL CHECK (kind IN ('preference','fact','experience','task_state','unverified')),
    content text NOT NULL CHECK (char_length(content) BETWEEN 1 AND 50000),
    source jsonb NOT NULL DEFAULT '{}',
    version integer NOT NULL DEFAULT 1 CHECK (version>0),
    expires_at timestamptz,
    deleted_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id,id),
    FOREIGN KEY (tenant_id,workspace_id)
        REFERENCES memshare.workspaces(tenant_id,id),
    FOREIGN KEY (tenant_id,workspace_id,project_id)
        REFERENCES memshare.projects(tenant_id,workspace_id,id),
    FOREIGN KEY (tenant_id,creator_principal_id)
        REFERENCES memshare.principals(tenant_id,id),
    FOREIGN KEY (tenant_id,owner_principal_id)
        REFERENCES memshare.principals(tenant_id,id),
    CHECK ((scope IN ('private','agent') AND owner_principal_id IS NOT NULL)
        OR (scope IN ('workspace','project') AND owner_principal_id IS NULL)),
    CHECK (scope NOT IN ('private','agent') OR owner_principal_id=scope_id),
    CHECK ((scope='project' AND project_id IS NOT NULL AND scope_id=project_id)
        OR (scope<>'project' AND project_id IS NULL)),
    CHECK ((scope='workspace' AND scope_id=workspace_id) OR scope<>'workspace')
);

CREATE TABLE memshare.memory_versions (
    tenant_id uuid NOT NULL,
    memory_id uuid NOT NULL,
    version integer NOT NULL CHECK (version>0),
    content text NOT NULL,
    changed_by uuid NOT NULL,
    changed_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id,memory_id,version),
    FOREIGN KEY (tenant_id,memory_id)
        REFERENCES memshare.memories(tenant_id,id),
    FOREIGN KEY (tenant_id,changed_by)
        REFERENCES memshare.principals(tenant_id,id)
);

CREATE TABLE memshare.idempotency (
    tenant_id uuid NOT NULL,
    principal_id uuid NOT NULL,
    operation text NOT NULL,
    key text NOT NULL CHECK (char_length(key) BETWEEN 1 AND 200),
    input_hash text NOT NULL CHECK (char_length(input_hash)=64),
    result_id uuid NOT NULL,
    expires_at timestamptz NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id,principal_id,operation,key),
    FOREIGN KEY (tenant_id,principal_id)
        REFERENCES memshare.principals(tenant_id,id)
);

CREATE TABLE memshare.audit_events (
    tenant_id uuid NOT NULL,
    id uuid NOT NULL,
    actor_principal_id uuid NOT NULL,
    action text NOT NULL,
    outcome text NOT NULL CHECK (outcome IN ('allowed','denied','failed')),
    target_type text NOT NULL,
    target_id uuid,
    request_id text,
    occurred_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id,id),
    FOREIGN KEY (tenant_id,actor_principal_id)
        REFERENCES memshare.principals(tenant_id,id)
);

CREATE INDEX memories_search_idx ON memshare.memories
    (tenant_id,workspace_id,updated_at DESC,id)
    WHERE deleted_at IS NULL;
CREATE INDEX grants_subject_idx ON memshare.grants
    (tenant_id,subject_principal_id,workspace_id,scope,scope_id)
    WHERE revoked_at IS NULL;
CREATE INDEX credentials_principal_idx ON memshare.credentials
    (tenant_id,principal_id,status);

CREATE FUNCTION memshare.authenticate_credential(p_token_hash text)
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
      JOIN memshare.tenants t ON t.id=c.tenant_id
     WHERE c.token_hash=p_token_hash
       AND c.status='active'
       AND (c.expires_at IS NULL OR c.expires_at>now())
       AND p.status='active'
       AND t.status='active'
$$;

REVOKE ALL ON FUNCTION memshare.authenticate_credential(text) FROM PUBLIC;

DO $$
DECLARE table_name text;
BEGIN
  FOREACH table_name IN ARRAY ARRAY[
    'workspaces','workspace_memberships','projects',
    'project_memberships','grants','memories','memory_versions','idempotency','audit_events'
  ] LOOP
    EXECUTE format('ALTER TABLE memshare.%I ENABLE ROW LEVEL SECURITY',table_name);
    EXECUTE format('ALTER TABLE memshare.%I FORCE ROW LEVEL SECURITY',table_name);
    EXECUTE format(
      'CREATE POLICY tenant_isolation ON memshare.%I USING (tenant_id=memshare.current_tenant_id()) WITH CHECK (tenant_id=memshare.current_tenant_id())',
      table_name
    );
  END LOOP;
END $$;

-- Authentication is a deliberately narrow pre-tenant operation. The schema owner
-- may read principals through the fixed SECURITY DEFINER function, while the
-- non-owner runtime role remains subject to this policy for ordinary DML.
ALTER TABLE memshare.principals ENABLE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON memshare.principals
USING (tenant_id=memshare.current_tenant_id())
WITH CHECK (tenant_id=memshare.current_tenant_id());

ALTER TABLE memshare.tenants ENABLE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON memshare.tenants
USING (id=memshare.current_tenant_id())
WITH CHECK (id=memshare.current_tenant_id());

-- Authentication runs through the fixed SECURITY DEFINER function. Runtime DML on
-- credentials is tenant-scoped; the migration owner is intentionally not forced
-- through this policy so the pre-tenant authentication lookup can resolve identity.
ALTER TABLE memshare.credentials ENABLE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON memshare.credentials
USING (tenant_id=memshare.current_tenant_id())
WITH CHECK (tenant_id=memshare.current_tenant_id());

-- The schema owner must differ from the NOBYPASSRLS runtime role; serve verifies
-- that boundary. The owner is reserved for migrations and operator provisioning.
