//! The gateway owns model execution. Readers may disappear without cancelling it.
use super::*;
use crate::vcp_modules::persistence::mobile_job_repository::{self as repository, MobileJob};
use base64::Engine;
use tokio_util::codec::Decoder;

fn jobs_endpoint(endpoint: &str) -> Result<(String, String), String> {
    let mut url = reqwest::Url::parse(endpoint).map_err(|_| "模型地址无效".to_string())?;
    let route = url.path().to_string();
    if !matches!(
        route.as_str(),
        "/v1/chat/completions" | "/v1/chatvcp/completions"
    ) || !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err("此安装包需要连接支持持久任务的 VCPMobile 网关".into());
    }
    url.set_path("/v1/mobile/jobs");
    url.set_query(None);
    url.set_fragment(None);
    Ok((url.to_string(), route))
}

pub(super) async fn prepare<R: Runtime>(
    app: &AppHandle<R>,
    key: &MessageKey,
    endpoint: &str,
    api_key: &str,
    mut body: Value,
    streaming: bool,
) -> Result<MobileJob, VcpRequestFailure> {
    let (endpoint, route) = jobs_endpoint(endpoint)?;
    let request_id = uuid::Uuid::new_v4().to_string();
    body.as_object_mut()
        .ok_or_else(|| "模型参数必须是对象".to_string())?
        .insert("requestId".into(), json!(request_id));
    let id = request_id.clone();
    let request_json = tokio::task::spawn_blocking(move || {
        serde_json::to_string(&json!({"requestId": id, "route": route, "body": body}))
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())??;
    if request_json.len() > 16 * 1024 * 1024 {
        return Err("本次请求超过持久任务的 16 MiB 上限，请减少附件或历史长度"
            .to_string()
            .into());
    }
    let job = MobileJob {
        request_id,
        endpoint,
        api_key: api_key.to_string(),
        request_json,
        streaming,
        disposition: "pending".into(),
    };
    repository::insert(&app.state::<DbState>(), key, &job).await?;
    Ok(job)
}

fn client() -> Client {
    // Bounded JSON reads use the existing non-redirecting profile. A redirect
    // must never move a persisted chat body to another origin.
    super::super::http_clients::client(super::super::http_clients::HttpProfile::AdminApi).clone()
}

async fn send(
    job: &MobileJob,
    method: reqwest::Method,
    suffix: &str,
    body: Option<&str>,
) -> Result<reqwest::Response, String> {
    let url = if suffix.is_empty() {
        job.endpoint.clone()
    } else {
        format!("{}/{}/{}", job.endpoint, job.request_id, suffix)
            .trim_end_matches('/')
            .to_string()
    };
    let mut request = client()
        .request(method, url)
        .bearer_auth(&job.api_key)
        .timeout(Duration::from_secs(30));
    if let Some(body) = body {
        request = request
            .header(CONTENT_TYPE, "application/json")
            .body(body.to_owned());
    }
    request
        .send()
        .await
        .map_err(|_| "电脑连接暂时中断，任务保留，正在自动重连".to_string())
}

async fn status(job: &MobileJob) -> Result<reqwest::Response, String> {
    client()
        .get(format!("{}/{}", job.endpoint, job.request_id))
        .bearer_auth(&job.api_key)
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|_| "电脑连接暂时中断，正在自动重连".to_string())
}

async fn bounded_json(job: &MobileJob, response: reqwest::Response) -> Result<Value, String> {
    if !response.status().is_success() {
        return Err(format!(
            "持久任务网关返回 HTTP {}，请检查电脑网关和连接配置",
            response.status().as_u16()
        ));
    }
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| "任务传输中断，将从已读取位置恢复".to_string())?;
        if bytes.len() + chunk.len() > 1024 * 1024 {
            return Err("任务响应超过传输上限".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    let value: Value =
        serde_json::from_slice(&bytes).map_err(|_| "任务响应不完整，将重新读取".to_string())?;
    if value["id"].as_str() != Some(job.request_id.as_str())
        || !matches!(
            value["state"].as_str(),
            Some(
                "queued"
                    | "running"
                    | "cancelling"
                    | "completed"
                    | "failed"
                    | "cancelled"
                    | "interrupted"
                    | "expired"
            )
        )
        || value["length"].as_u64().is_none()
    {
        return Err("任务响应身份或状态不一致，保留任务等待重连".into());
    }
    Ok(value)
}

// Never replay a POST merely because its response was lost. Query first; an
// explicit 404 allows the identical saved request and ID to be submitted again.
async fn ensure_owned(job: &MobileJob, stopping: bool) -> Result<(), String> {
    if stopping {
        bounded_json(job, send(job, reqwest::Method::POST, "cancel", None).await?).await?;
        return Ok(());
    }
    let response = status(job).await?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        bounded_json(
            job,
            send(job, reqwest::Method::POST, "", Some(&job.request_json)).await?,
        )
        .await?;
    } else {
        bounded_json(job, response).await?;
    }
    Ok(())
}

async fn page(job: &MobileJob, offset: u64) -> Result<Value, String> {
    bounded_json(
        job,
        send(
            job,
            reqwest::Method::GET,
            &format!("bytes?offset={offset}&limit=65536"),
            None,
        )
        .await?,
    )
    .await
}

fn reconnecting(
    output: &RequestOutput,
    message_id: &str,
    context: &Option<Value>,
    message: String,
) {
    output.send(StreamEvent {
        r#type: "reconnecting".into(),
        message_id: message_id.into(),
        context: context.clone(),
        error: Some(message),
        ..Default::default()
    });
}

fn detached(message: impl Into<String>, partial: &str, stopping: bool) -> VcpRequestFailure {
    VcpRequestFailure {
        message: message.into(),
        partial_content: Some(partial.into()),
        recovery_pending: true,
        stop_pending: stopping,
    }
}

pub(super) async fn run(
    job: MobileJob,
    message_id: String,
    context: Option<Value>,
    cancellation: CancellationToken,
    output: Arc<RequestOutput>,
) -> Result<VcpResponse, VcpRequestFailure> {
    let _slot = CHAT_TRANSPORT_SLOTS
        .try_acquire()
        .map_err(|_| VcpRequestFailure {
            message: STREAM_CAPACITY_EXHAUSTED.into(),
            partial_content: None,
            recovery_pending: true,
            stop_pending: cancellation.is_cancelled(),
        })?;
    let mut aurora = AuroraBuffer::new();
    let mut decoder = SseDecoder::default();
    let mut bytes = bytes::BytesMut::new();
    let mut pending = String::new();
    let mut finish = None;
    let mut parse_error = None;
    let mut offset = 0u64;
    let mut admitted = false;
    let mut stop_sent = false;
    let mut failures = 0u32;
    loop {
        if output.state.lock().is_ok_and(|state| state.discarded) {
            return Err(detached(
                "会话已删除，远端任务交由清理队列停止",
                &aurora.full_text,
                true,
            ));
        }
        if !admitted || (cancellation.is_cancelled() && !stop_sent) {
            let stopping_attempt = cancellation.is_cancelled();
            match ensure_owned(&job, stopping_attempt).await {
                Ok(()) => {
                    admitted = true;
                    stop_sent = stopping_attempt;
                    failures = 0;
                }
                Err(error) => {
                    reconnecting(&output, &message_id, &context, error);
                    failures = failures.saturating_add(1);
                    tokio::time::sleep(Duration::from_secs(u64::from(failures.min(6)) * 5)).await;
                    continue;
                }
            }
        }
        let next = page(&job, offset);
        tokio::pin!(next);
        let response = loop {
            tokio::select! {
                value = &mut next => break value,
                _ = output.changed.notified() => output.send_aurora(&mut aurora, &message_id, &context, None, false),
                _ = cancellation.cancelled(), if !stop_sent => break Err("正在停止电脑任务".into()),
            }
        };
        let value = match response {
            Ok(value) => {
                failures = 0;
                value
            }
            Err(error) => {
                if cancellation.is_cancelled() && !stop_sent {
                    continue;
                }
                reconnecting(&output, &message_id, &context, error);
                failures = failures.saturating_add(1);
                tokio::time::sleep(Duration::from_secs(u64::from(failures.min(6)) * 5)).await;
                continue;
            }
        };
        let raw = base64::engine::general_purpose::STANDARD
            .decode(value["data"].as_str().unwrap_or_default())
            .map_err(|_| {
                detached(
                    "任务数据编码无效，下次连接将重新读取",
                    &aurora.full_text,
                    cancellation.is_cancelled(),
                )
            })?;
        let next_offset = value["nextOffset"].as_u64();
        if value["offset"].as_u64() != Some(offset)
            || next_offset != Some(offset + raw.len() as u64)
            || value["length"].as_u64() < next_offset
            || offset + raw.len() as u64 > 64 * 1024 * 1024
        {
            return Err(detached(
                "任务数据位置不一致，下次连接将重新读取",
                &aurora.full_text,
                cancellation.is_cancelled(),
            ));
        }
        offset += raw.len() as u64;
        bytes.extend_from_slice(&raw);
        if job.streaming {
            while let Some(data) = decoder.decode(&mut bytes).map_err(|e| {
                detached(
                    e.to_string(),
                    &aurora.full_text,
                    cancellation.is_cancelled(),
                )
            })? {
                if let Err(error) = absorb_sse_data(&data, &mut pending, &mut finish) {
                    parse_error = Some(error);
                }
            }
            if bytes.len() > 4 * 1024 * 1024 {
                return Err(detached(
                    "单条流式事件过大",
                    &aurora.full_text,
                    cancellation.is_cancelled(),
                ));
            }
            flush_aurora_window(&mut aurora, &mut pending);
            output.send_aurora(&mut aurora, &message_id, &context, None, false);
        }
        let state = value["state"].as_str().unwrap_or_default();
        let terminal = matches!(
            state,
            "completed" | "failed" | "cancelled" | "interrupted" | "expired"
        );
        if !terminal || value["length"].as_u64() != Some(offset) {
            continue;
        }
        if !job.streaming && state == "completed" {
            let response: Value = serde_json::from_slice(&bytes).map_err(|_| {
                detached(
                    "电脑返回的 JSON 无效，下次连接将重新读取",
                    &aurora.full_text,
                    cancellation.is_cancelled(),
                )
            })?;
            if let Some(content) = response["choices"][0]["message"]["content"].as_str() {
                aurora.append_chunk(content);
            }
            finish = response["choices"][0]["finish_reason"]
                .as_str()
                .map(str::to_string);
        }
        aurora.finalize();
        output.send_aurora(&mut aurora, &message_id, &context, None, false);
        if state == "cancelled" {
            return Ok(VcpResponse {
                full_content: aurora.full_text,
                finish_reason: Some("cancelled_by_user".into()),
                is_aborted: true,
            });
        }
        if state != "completed" || parse_error.is_some() {
            return Err(VcpRequestFailure::streaming(
                parse_error.unwrap_or_else(|| {
                    format!(
                        "电脑任务结束：{}（{}）",
                        state,
                        value["reason"].as_str().unwrap_or("unknown")
                    )
                }),
                aurora.full_text,
            ));
        }
        return Ok(VcpResponse {
            full_content: aurora.full_text,
            finish_reason: Some(
                finish
                    .filter(|x| x != "stop")
                    .unwrap_or_else(|| "completed".into()),
            ),
            is_aborted: false,
        });
    }
}

pub(super) async fn recover<R: Runtime>(
    app: &AppHandle<R>,
    key: &MessageKey,
    job: MobileJob,
    channel: Channel<StreamEvent>,
    cancellation: CancellationToken,
    agent_id: Option<String>,
) -> Result<RecoveryStatus, String> {
    let context = Some(
        json!({"ownerType": key.topic.owner_type, "ownerId": key.topic.owner_id,
        "topicId": key.topic.topic_id, "agentId": agent_id, "replayed": true}),
    );
    let result = run(
        job,
        key.msg_id.clone(),
        context,
        cancellation,
        request_output(app, key, Some(channel.clone())),
    )
    .await;
    let db = app.state::<DbState>();
    match result {
        Ok(response) => {
            let committed =
                crate::vcp_modules::chat::message_service::finalize_recovered_stream_message(
                    app.clone(),
                    &db.pool,
                    key,
                    response.full_content,
                    if response.is_aborted {
                        "cancelled_by_user".into()
                    } else {
                        response.finish_reason.unwrap_or_else(|| "completed".into())
                    },
                    channel,
                    agent_id,
                )
                .await?;
            Ok(if committed.is_some() {
                RecoveryStatus::Completed
            } else {
                RecoveryStatus::NotFound
            })
        }
        Err(failure) if failure.is_recovery_pending() => Err(failure.to_string()),
        Err(failure) => {
            let (message, partial) = failure.into_parts();
            crate::vcp_modules::chat::message_service::finalize_recovered_stream_message(
                app.clone(),
                &db.pool,
                key,
                stream_error_content(&partial.unwrap_or_default(), &message),
                "error".into(),
                channel,
                agent_id,
            )
            .await?;
            Ok(RecoveryStatus::Failed)
        }
    }
}

pub(crate) async fn settle<R: Runtime>(app: &AppHandle<R>, key: &MessageKey, committed: bool) {
    if !cfg!(target_os = "ios") {
        return;
    }
    if let Some(db) = app.try_state::<DbState>() {
        if let Err(error) = repository::settle(&db, key, committed).await {
            log::warn!("[MobileJobs] Cleanup intent: {error}");
        }
    }
    crate::vcp_modules::sync::auto_sync::request(app);
}

pub(crate) async fn cleanup<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
    let db = app.state::<DbState>();
    for job in repository::cleanup_jobs(&db).await? {
        if !matches!(job.disposition.as_str(), "committed" | "discarded") {
            return Err("任务清理意图无效".into());
        }
        // Also cancels orphaned executions after local history is deleted.
        // A terminal job is unchanged by cancel; ACK is only sent after DB commit.
        bounded_json(
            &job,
            send(&job, reqwest::Method::POST, "cancel", None).await?,
        )
        .await?;
        bounded_json(&job, send(&job, reqwest::Method::POST, "ack", None).await?).await?;
        repository::remove(&db, &job.request_id).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn gateway_fixture(
        mode: &'static str,
        cancellation: CancellationToken,
    ) -> (
        MobileJob,
        Arc<AtomicBool>,
        Arc<AtomicUsize>,
        tokio::task::JoinHandle<()>,
    ) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let cancelled = Arc::new(AtomicBool::new(false));
        let posts = Arc::new(AtomicUsize::new(0));
        let cancel_seen = cancelled.clone();
        let post_count = posts.clone();
        let worker = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let cancelled = cancel_seen.clone();
                let posts = post_count.clone();
                let cancellation = cancellation.clone();
                tokio::spawn(async move {
                    let mut input = Vec::new();
                    let mut chunk = [0u8; 4096];
                    let head = loop {
                        let n = socket.read(&mut chunk).await.unwrap();
                        if n == 0 {
                            return;
                        }
                        input.extend_from_slice(&chunk[..n]);
                        if let Some(end) = input.windows(4).position(|x| x == b"\r\n\r\n") {
                            let head = String::from_utf8_lossy(&input[..end]).to_string();
                            let length: usize = head
                                .lines()
                                .find_map(|line| {
                                    line.to_ascii_lowercase()
                                        .strip_prefix("content-length: ")
                                        .and_then(|x| x.parse().ok())
                                })
                                .unwrap_or(0);
                            if input.len() >= end + 4 + length {
                                break head;
                            }
                        }
                    };
                    let route = head.lines().next().unwrap();
                    assert!(head
                        .to_ascii_lowercase()
                        .contains("authorization: bearer fixture-key"));
                    let stream = "data: {\"choices\":[{\"delta\":{\"content\":\"你好🌏\"}}]}\n\ndata: [DONE]\n\n";
                    let mut status = 200;
                    let state = if mode == "cancel-admission" {
                        if route.starts_with("POST /v1/mobile/jobs ") {
                            posts.fetch_add(1, Ordering::SeqCst);
                            cancellation.cancel();
                            "running"
                        } else if route.contains("/cancel ") {
                            cancelled.store(true, Ordering::SeqCst);
                            "cancelled"
                        } else if cancelled.load(Ordering::SeqCst) {
                            "cancelled"
                        } else {
                            status = 404;
                            "running"
                        }
                    } else {
                        "completed"
                    };
                    if route.starts_with("POST") && mode != "cancel-admission" {
                        posts.fetch_add(1, Ordering::SeqCst);
                    }
                    let data = if mode == "cancel-admission" {
                        "".to_string()
                    } else if mode == "bad-page" {
                        "%%%".into()
                    } else {
                        base64::engine::general_purpose::STANDARD.encode(stream)
                    };
                    let length = if mode == "cancel-admission" || mode == "bad-page" {
                        0
                    } else {
                        stream.len()
                    };
                    let body = json!({"id":"fixture-durable-job", "state":state, "length":length,
                        "offset":0, "nextOffset":length, "data":data})
                    .to_string();
                    let response = format!("HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                    let _ = socket.write_all(response.as_bytes()).await;
                });
            }
        });
        let job = MobileJob {
            request_id: "fixture-durable-job".into(),
            endpoint: format!("http://{address}/v1/mobile/jobs"),
            api_key: "fixture-key".into(),
            request_json: "{}".into(),
            streaming: true,
            disposition: "pending".into(),
        };
        (job, cancelled, posts, worker)
    }

    #[tokio::test]
    async fn cancel_while_admission_is_in_flight_still_sends_remote_cancel() {
        let token = CancellationToken::new();
        let (job, cancelled, posts, worker) =
            gateway_fixture("cancel-admission", token.clone()).await;
        let result = tokio::time::timeout(
            Duration::from_secs(10),
            run(
                job,
                "m".into(),
                None,
                token,
                Arc::new(RequestOutput::default()),
            ),
        )
        .await;
        worker.abort();
        assert!(result.unwrap().unwrap().is_aborted);
        assert!(cancelled.load(Ordering::SeqCst));
        assert_eq!(posts.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn cold_reader_replays_existing_result_without_resubmitting() {
        let token = CancellationToken::new();
        let (job, _, posts, worker) = gateway_fixture("completed", token.clone()).await;
        let result = tokio::time::timeout(
            Duration::from_secs(10),
            run(
                job,
                "m".into(),
                None,
                token,
                Arc::new(RequestOutput::default()),
            ),
        )
        .await;
        worker.abort();
        assert_eq!(result.unwrap().unwrap().full_content, "你好🌏");
        assert_eq!(posts.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn local_decode_failure_keeps_remote_result_recoverable() {
        let token = CancellationToken::new();
        let (job, _, posts, worker) = gateway_fixture("bad-page", token.clone()).await;
        let result = tokio::time::timeout(
            Duration::from_secs(10),
            run(
                job,
                "m".into(),
                None,
                token,
                Arc::new(RequestOutput::default()),
            ),
        )
        .await;
        worker.abort();
        assert!(result.unwrap().unwrap_err().is_recovery_pending());
        assert_eq!(posts.load(Ordering::SeqCst), 0);
    }
    #[test]
    fn jobs_stay_on_the_configured_origin() {
        let (endpoint, route) = jobs_endpoint("https://phone.example/v1/chat/completions").unwrap();
        assert_eq!(endpoint, "https://phone.example/v1/mobile/jobs");
        assert_eq!(route, "/v1/chat/completions");
        assert!(jobs_endpoint("https://user:secret@phone.example/v1/chat/completions").is_err());
        assert!(jobs_endpoint("https://phone.example/admin").is_err());
    }
}
