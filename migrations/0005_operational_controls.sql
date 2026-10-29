CREATE EXTENSION IF NOT EXISTS pgcrypto;

ALTER TABLE memshare.audit_events
    ADD COLUMN credential_id uuid REFERENCES memshare.credentials(id),
    ADD COLUMN chain_position bigint CHECK (chain_position>0),
    ADD COLUMN previous_hash text CHECK (char_length(previous_hash)=64),
    ADD COLUMN event_hash text CHECK (char_length(event_hash)=64);

CREATE INDEX audit_events_time_idx ON memshare.audit_events
    (tenant_id, occurred_at DESC, id DESC);

CREATE TABLE memshare.audit_chain_heads (
    tenant_id uuid PRIMARY KEY REFERENCES memshare.tenants(id),
    last_hash text NOT NULL CHECK (char_length(last_hash)=64),
    last_position bigint NOT NULL DEFAULT 0 CHECK (last_position>=0),
    updated_at timestamptz NOT NULL DEFAULT clock_timestamp()
);

ALTER TABLE memshare.audit_chain_heads ENABLE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON memshare.audit_chain_heads
USING (tenant_id=memshare.current_tenant_id())
WITH CHECK (tenant_id=memshare.current_tenant_id());

ALTER TABLE memshare.audit_events NO FORCE ROW LEVEL SECURITY;

DO $$
DECLARE
    tenant_row record;
    event_row record;
    previous_value text;
    event_value text;
    event_position bigint;
BEGIN
    FOR tenant_row IN SELECT id FROM memshare.tenants ORDER BY id LOOP
        previous_value := repeat('0',64);
        event_position := 0;
        FOR event_row IN
            SELECT * FROM memshare.audit_events
             WHERE tenant_id=tenant_row.id
             ORDER BY occurred_at,id
        LOOP
            event_position := event_position + 1;
            event_value := encode(public.digest(convert_to(concat_ws('|',
                event_row.tenant_id::text,
                event_row.id::text,
                event_row.actor_principal_id::text,
                COALESCE(event_row.credential_id::text,''),
                event_row.action,
                event_row.outcome,
                event_row.target_type,
                COALESCE(event_row.target_id::text,''),
                COALESCE(event_row.request_id,''),
                event_position::text,
                event_row.occurred_at::text,
                previous_value
            ),'UTF8'),'sha256'),'hex');
            UPDATE memshare.audit_events
               SET chain_position=event_position,previous_hash=previous_value,event_hash=event_value
             WHERE tenant_id=event_row.tenant_id AND id=event_row.id;
            previous_value := event_value;
        END LOOP;
        INSERT INTO memshare.audit_chain_heads(tenant_id,last_hash,last_position)
        VALUES (tenant_row.id,previous_value,event_position)
        ON CONFLICT (tenant_id) DO UPDATE
          SET last_hash=EXCLUDED.last_hash,last_position=EXCLUDED.last_position;
    END LOOP;
END
$$;

ALTER TABLE memshare.audit_events
    ALTER COLUMN chain_position SET NOT NULL,
    ALTER COLUMN previous_hash SET NOT NULL,
    ALTER COLUMN event_hash SET NOT NULL;
CREATE UNIQUE INDEX audit_events_chain_position_idx
    ON memshare.audit_events(tenant_id,chain_position);
ALTER TABLE memshare.audit_events FORCE ROW LEVEL SECURITY;

CREATE FUNCTION memshare.chain_audit_event()
RETURNS trigger
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, memshare
AS $$
DECLARE
    previous_value text;
    previous_position bigint;
BEGIN
    INSERT INTO memshare.audit_chain_heads(tenant_id,last_hash,last_position)
    VALUES (NEW.tenant_id,repeat('0',64),0)
    ON CONFLICT (tenant_id) DO NOTHING;

    SELECT last_hash,last_position INTO previous_value,previous_position
      FROM memshare.audit_chain_heads
     WHERE tenant_id=NEW.tenant_id
     FOR UPDATE;

    NEW.chain_position := previous_position + 1;
    NEW.previous_hash := previous_value;
    NEW.event_hash := encode(public.digest(convert_to(concat_ws('|',
        NEW.tenant_id::text,
        NEW.id::text,
        NEW.actor_principal_id::text,
        COALESCE(NEW.credential_id::text,''),
        NEW.action,
        NEW.outcome,
        NEW.target_type,
        COALESCE(NEW.target_id::text,''),
        COALESCE(NEW.request_id,''),
        NEW.chain_position::text,
        NEW.occurred_at::text,
        previous_value
    ),'UTF8'),'sha256'),'hex');

    UPDATE memshare.audit_chain_heads
       SET last_hash=NEW.event_hash,last_position=NEW.chain_position,updated_at=clock_timestamp()
     WHERE tenant_id=NEW.tenant_id;
    RETURN NEW;
END
$$;

REVOKE ALL ON FUNCTION memshare.chain_audit_event() FROM PUBLIC;

CREATE TRIGGER audit_event_chain
BEFORE INSERT ON memshare.audit_events
FOR EACH ROW EXECUTE FUNCTION memshare.chain_audit_event();

CREATE TABLE memshare.tenant_limits (
    tenant_id uuid PRIMARY KEY REFERENCES memshare.tenants(id),
    max_principals bigint NOT NULL DEFAULT 1000 CHECK (max_principals BETWEEN 1 AND 1000000),
    max_memories bigint NOT NULL DEFAULT 100000 CHECK (max_memories BETWEEN 1 AND 100000000),
    max_handoffs bigint NOT NULL DEFAULT 100000 CHECK (max_handoffs BETWEEN 1 AND 100000000),
    requests_per_minute bigint NOT NULL DEFAULT 6000 CHECK (requests_per_minute BETWEEN 1 AND 10000000),
    updated_at timestamptz NOT NULL DEFAULT now()
);

INSERT INTO memshare.tenant_limits(tenant_id)
SELECT id FROM memshare.tenants
ON CONFLICT (tenant_id) DO NOTHING;

CREATE TABLE memshare.request_counters (
    tenant_id uuid NOT NULL REFERENCES memshare.tenants(id),
    credential_id uuid NOT NULL REFERENCES memshare.credentials(id),
    window_start timestamptz NOT NULL,
    request_count bigint NOT NULL CHECK (request_count > 0),
    PRIMARY KEY (tenant_id, credential_id, window_start)
);

ALTER TABLE memshare.tenant_limits ENABLE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON memshare.tenant_limits
USING (tenant_id=memshare.current_tenant_id())
WITH CHECK (tenant_id=memshare.current_tenant_id());

ALTER TABLE memshare.request_counters ENABLE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON memshare.request_counters
USING (tenant_id=memshare.current_tenant_id())
WITH CHECK (tenant_id=memshare.current_tenant_id());

CREATE FUNCTION memshare.authenticate_request(p_token_hash text)
RETURNS TABLE(
    credential_id uuid,
    tenant_id uuid,
    principal_id uuid,
    kind text,
    rate_allowed boolean,
    rate_limit bigint,
    rate_remaining bigint,
    rate_reset_at timestamptz
)
LANGUAGE plpgsql
SECURITY DEFINER
VOLATILE
SET search_path = pg_catalog, memshare
AS $$
DECLARE
    identity_row record;
    limit_value bigint;
    count_value bigint;
    window_value timestamptz := date_trunc('minute', clock_timestamp());
BEGIN
    SELECT c.id AS credential_id, c.tenant_id, c.principal_id, p.kind
      INTO identity_row
      FROM memshare.credentials c
      JOIN memshare.principals p
        ON p.tenant_id=c.tenant_id AND p.id=c.principal_id
      LEFT JOIN memshare.principals owner
        ON owner.tenant_id=p.tenant_id AND owner.id=p.owner_principal_id
      JOIN memshare.tenants t ON t.id=c.tenant_id
     WHERE c.token_hash=p_token_hash
       AND c.status='active'
       AND (c.expires_at IS NULL OR c.expires_at>clock_timestamp())
       AND p.status='active'
       AND (p.kind<>'agent' OR owner.status='active')
       AND t.status='active';

    IF NOT FOUND THEN
        RETURN;
    END IF;

    SELECT requests_per_minute INTO limit_value
      FROM memshare.tenant_limits
     WHERE tenant_limits.tenant_id=identity_row.tenant_id;
    limit_value := COALESCE(limit_value, 6000);

    INSERT INTO memshare.request_counters(tenant_id,credential_id,window_start,request_count)
    VALUES (identity_row.tenant_id,identity_row.credential_id,window_value,1)
    ON CONFLICT ON CONSTRAINT request_counters_pkey
    DO UPDATE SET request_count=memshare.request_counters.request_count+1
    RETURNING request_count INTO count_value;

    UPDATE memshare.credentials
       SET last_used_at=clock_timestamp()
     WHERE id=identity_row.credential_id
       AND (last_used_at IS NULL OR last_used_at<clock_timestamp()-interval '1 minute');

    DELETE FROM memshare.request_counters
     WHERE request_counters.tenant_id=identity_row.tenant_id
       AND window_start<window_value-interval '2 hours';

    RETURN QUERY SELECT
        identity_row.credential_id,
        identity_row.tenant_id,
        identity_row.principal_id,
        identity_row.kind,
        count_value<=limit_value,
        limit_value,
        GREATEST(limit_value-count_value,0),
        window_value+interval '1 minute';
END
$$;

REVOKE ALL ON FUNCTION memshare.authenticate_request(text) FROM PUBLIC;
