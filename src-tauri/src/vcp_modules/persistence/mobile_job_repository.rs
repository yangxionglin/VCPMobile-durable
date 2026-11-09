//! Durable network intent is committed before contacting the gateway.
use super::db_manager::DbState;
use crate::vcp_modules::topic_types::MessageKey;

#[derive(Clone, sqlx::FromRow)]
pub(crate) struct MobileJob {
    pub request_id: String,
    pub endpoint: String,
    pub api_key: String,
    pub request_json: String,
    pub streaming: bool,
    pub disposition: String,
}

pub(crate) async fn insert(db: &DbState, key: &MessageKey, job: &MobileJob) -> Result<(), String> {
    let mut tx = db.write_transaction("mobile_job.prepare").await?;
    sqlx::query("INSERT INTO mobile_job_outbox (request_id, owner_type, owner_id, topic_id, msg_id, endpoint, api_key, request_json, streaming, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)")
        .bind(&job.request_id).bind(&key.topic.owner_type).bind(&key.topic.owner_id)
        .bind(&key.topic.topic_id).bind(&key.msg_id).bind(&job.endpoint).bind(&job.api_key)
        .bind(&job.request_json).bind(job.streaming).bind(chrono::Utc::now().timestamp_millis())
        .execute(&mut *tx).await.map_err(|e| e.to_string())?;
    tx.commit().await
}

pub(crate) async fn pending(db: &DbState, key: &MessageKey) -> Result<Option<MobileJob>, String> {
    sqlx::query_as("SELECT request_id, endpoint, api_key, request_json, streaming, disposition FROM mobile_job_outbox WHERE owner_type=? AND owner_id=? AND topic_id=? AND msg_id=? AND disposition='pending' ORDER BY created_at DESC, rowid DESC LIMIT 1")
        .bind(&key.topic.owner_type).bind(&key.topic.owner_id).bind(&key.topic.topic_id).bind(&key.msg_id)
        .fetch_optional(&db.pool).await.map_err(|e| e.to_string())
}

pub(crate) async fn settle(db: &DbState, key: &MessageKey, committed: bool) -> Result<(), String> {
    let mut tx = db.write_transaction("mobile_job.settle").await?;
    sqlx::query("UPDATE mobile_job_outbox SET disposition=? WHERE owner_type=? AND owner_id=? AND topic_id=? AND msg_id=? AND disposition='pending'")
        .bind(if committed { "committed" } else { "discarded" })
        .bind(&key.topic.owner_type).bind(&key.topic.owner_id).bind(&key.topic.topic_id).bind(&key.msg_id)
        .execute(&mut *tx).await.map_err(|e| e.to_string())?;
    tx.commit().await
}

// Reconcile a crash between the authoritative message commit and its cleanup hook.
pub(crate) async fn cleanup_jobs(db: &DbState) -> Result<Vec<MobileJob>, String> {
    let mut tx = db.write_transaction("mobile_job.reconcile").await?;
    sqlx::query("UPDATE mobile_job_outbox AS j SET disposition=CASE WHEN EXISTS (SELECT 1 FROM messages m WHERE m.owner_type=j.owner_type AND m.owner_id=j.owner_id AND m.topic_id=j.topic_id AND m.msg_id=j.msg_id AND m.deleted_at IS NULL AND m.finish_reason IS NOT NULL) THEN 'committed' ELSE 'discarded' END WHERE disposition='pending' AND NOT EXISTS (SELECT 1 FROM active_generations ag WHERE ag.owner_type=j.owner_type AND ag.owner_id=j.owner_id AND ag.topic_id=j.topic_id AND ag.msg_id=j.msg_id)")
        .execute(&mut *tx).await.map_err(|e| e.to_string())?;
    tx.commit().await?;
    sqlx::query_as("SELECT request_id, endpoint, api_key, request_json, streaming, disposition FROM mobile_job_outbox WHERE disposition!='pending' LIMIT 32")
        .fetch_all(&db.pool).await.map_err(|e| e.to_string())
}

pub(crate) async fn remove(db: &DbState, id: &str) -> Result<(), String> {
    let mut tx = db.write_transaction("mobile_job.acknowledge").await?;
    sqlx::query("DELETE FROM mobile_job_outbox WHERE request_id=? AND disposition!='pending'")
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;
    tx.commit().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vcp_modules::topic_types::TopicKey;

    #[tokio::test]
    async fn committed_and_deleted_messages_keep_cleanup_intent_across_crash() {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::raw_sql(include_str!("../../../migrations/0100_baseline_v2.sql"))
            .execute(&pool)
            .await
            .unwrap();
        sqlx::raw_sql(include_str!(
            "../../../migrations/0102_stream_stop_intent.sql"
        ))
        .execute(&pool)
        .await
        .unwrap();
        sqlx::raw_sql(include_str!(
            "../../../migrations/0106_durable_mobile_jobs.sql"
        ))
        .execute(&pool)
        .await
        .unwrap();
        sqlx::raw_sql("INSERT INTO agents(owner_type,agent_id,name,model,updated_at) VALUES('agent','a','A','test',1); INSERT INTO topics(topic_id,owner_type,owner_id,title,created_at,updated_at) VALUES('t','agent','a','T',1,1);").execute(&pool).await.unwrap();
        let db = DbState::new(pool, std::path::PathBuf::new());
        let key = MessageKey::new(TopicKey::new("agent", "a", "t"), "m");
        crate::vcp_modules::chat::message_service::begin_stream_message(
            &db,
            &key,
            Some("a"),
            Some("A"),
        )
        .await
        .unwrap();
        let job = MobileJob {
            request_id: "test-durable-job".into(),
            endpoint: "https://example.test/v1/mobile/jobs".into(),
            api_key: "fixture-key".into(),
            request_json: "{\"body\":\"你好🌏\"}".into(),
            streaming: true,
            disposition: "pending".into(),
        };
        insert(&db, &key, &job).await.unwrap();
        assert_eq!(
            pending(&db, &key).await.unwrap().unwrap().request_json,
            job.request_json
        );
        assert!(cleanup_jobs(&db).await.unwrap().is_empty());
        remove(&db, &job.request_id).await.unwrap();
        assert!(pending(&db, &key).await.unwrap().is_some());
        // Simulate a crash immediately after the canonical message transaction.
        sqlx::raw_sql("UPDATE messages SET finish_reason='completed' WHERE msg_id='m'; DELETE FROM active_generations WHERE msg_id='m';").execute(&db.pool).await.unwrap();
        let cleanup = cleanup_jobs(&db).await.unwrap();
        assert_eq!(cleanup.len(), 1);
        assert_eq!(cleanup[0].disposition, "committed");
        assert!(pending(&db, &key).await.unwrap().is_none());
        remove(&db, &job.request_id).await.unwrap();
        assert!(cleanup_jobs(&db).await.unwrap().is_empty());
        let deleted_key = MessageKey::new(key.topic.clone(), "deleted-message");
        insert(&db, &deleted_key, &job).await.unwrap();
        assert_eq!(cleanup_jobs(&db).await.unwrap()[0].disposition, "discarded");
    }
}
