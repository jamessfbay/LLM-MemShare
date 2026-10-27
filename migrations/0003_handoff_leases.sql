ALTER TABLE memshare.handoffs
    ADD COLUMN lease_expires_at timestamptz,
    ADD COLUMN cancelled_at timestamptz;

ALTER TABLE memshare.handoffs DROP CONSTRAINT handoffs_status_check;
ALTER TABLE memshare.handoffs DROP CONSTRAINT handoffs_check1;
ALTER TABLE memshare.handoffs DROP CONSTRAINT handoffs_check2;

ALTER TABLE memshare.handoffs ADD CONSTRAINT handoffs_status_check
    CHECK (status IN ('queued','claimed','completed','cancelled'));
ALTER TABLE memshare.handoffs ADD CONSTRAINT handoffs_state_check CHECK (
    (status='queued' AND claimed_at IS NULL AND lease_expires_at IS NULL
        AND completed_at IS NULL AND cancelled_at IS NULL)
 OR (status='claimed' AND claimed_at IS NOT NULL AND lease_expires_at IS NOT NULL
        AND completed_at IS NULL AND cancelled_at IS NULL)
 OR (status='completed' AND claimed_at IS NOT NULL AND lease_expires_at IS NOT NULL
        AND completed_at IS NOT NULL AND cancelled_at IS NULL)
 OR (status='cancelled' AND completed_at IS NULL AND cancelled_at IS NOT NULL)
);
ALTER TABLE memshare.handoffs ADD CONSTRAINT handoffs_result_check
    CHECK ((status='completed') = (result IS NOT NULL));

CREATE INDEX handoffs_inbox_idx ON memshare.handoffs
    (tenant_id,target_agent_id,status,created_at DESC,id DESC);
