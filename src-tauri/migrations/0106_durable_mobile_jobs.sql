-- Local transport intent, not part of the cross-device conversation schema.
-- No foreign key: deletion/commit must retain the remote cleanup intent.
CREATE TABLE mobile_job_outbox (
    request_id TEXT PRIMARY KEY,
    owner_type TEXT NOT NULL,
    owner_id TEXT NOT NULL,
    topic_id TEXT NOT NULL,
    msg_id TEXT NOT NULL,
    endpoint TEXT NOT NULL,
    api_key TEXT NOT NULL,
    request_json TEXT NOT NULL,
    streaming INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    disposition TEXT NOT NULL DEFAULT 'pending'
        CHECK (disposition IN ('pending', 'committed', 'discarded'))
);
CREATE INDEX mobile_job_message ON mobile_job_outbox(owner_type, owner_id, topic_id, msg_id, created_at);
