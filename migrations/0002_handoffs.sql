ALTER TABLE memshare.grants DROP CONSTRAINT grants_actions_check;
ALTER TABLE memshare.grants ADD CONSTRAINT grants_actions_check
    CHECK (cardinality(actions)>0 AND actions <@ ARRAY['read','write','delete','review','handoff']::text[]);

CREATE TABLE memshare.handoffs (
    tenant_id uuid NOT NULL,
    id uuid NOT NULL,
    workspace_id uuid NOT NULL,
    source_agent_id uuid NOT NULL,
    target_agent_id uuid NOT NULL,
    status text NOT NULL CHECK (status IN ('queued','claimed','completed')),
    task jsonb NOT NULL,
    input_memory_ids uuid[] NOT NULL DEFAULT '{}',
    result jsonb,
    result_memory_ids uuid[] NOT NULL DEFAULT '{}',
    source_metadata jsonb NOT NULL,
    result_metadata jsonb,
    version integer NOT NULL DEFAULT 1 CHECK (version>0),
    expires_at timestamptz,
    claimed_at timestamptz,
    completed_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id,id),
    FOREIGN KEY (tenant_id,workspace_id)
        REFERENCES memshare.workspaces(tenant_id,id),
    FOREIGN KEY (tenant_id,source_agent_id)
        REFERENCES memshare.principals(tenant_id,id),
    FOREIGN KEY (tenant_id,target_agent_id)
        REFERENCES memshare.principals(tenant_id,id),
    CHECK (source_agent_id<>target_agent_id),
    CHECK ((status='queued' AND claimed_at IS NULL AND completed_at IS NULL)
        OR (status='claimed' AND claimed_at IS NOT NULL AND completed_at IS NULL)
        OR (status='completed' AND claimed_at IS NOT NULL AND completed_at IS NOT NULL)),
    CHECK ((status='completed') = (result IS NOT NULL))
);

CREATE INDEX handoffs_participant_idx ON memshare.handoffs
    (tenant_id,workspace_id,target_agent_id,status,updated_at DESC);

ALTER TABLE memshare.handoffs ENABLE ROW LEVEL SECURITY;
ALTER TABLE memshare.handoffs FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON memshare.handoffs
USING (tenant_id=memshare.current_tenant_id())
WITH CHECK (tenant_id=memshare.current_tenant_id());
