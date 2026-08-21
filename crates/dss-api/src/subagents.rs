//! Durable local sub-agent execution backed by Frame/Run/RunAttempt records.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{SecondsFormat, Utc};
use dss_db::harness::{
    accept_frame_run, cancel_active_frame, collect_child_results, create_child_frame, get_frame,
    list_frame_messages, list_frame_tree, settle_frame_run, AcceptFrameRun, FrameRunMessage,
    NewChildFrame,
};
use dss_db::{DbError, DbPool};
use dss_llm::{ChatMessage, ChatRequest, LlmClient};
use dss_tools::SubagentRuntime;
use serde_json::{json, Value};
use tokio::sync::{oneshot, Mutex};
use tokio::task::AbortHandle;
use uuid::Uuid;

#[derive(Default)]
pub(crate) struct SubagentTaskRegistry {
    tasks: Mutex<HashMap<String, AbortHandle>>,
}

#[derive(Clone)]
pub(crate) struct DurableSubagentRuntime {
    db: Arc<DbPool>,
    llm: Arc<dyn LlmClient>,
    model: String,
    parent_frame_id: String,
    workspace: String,
    registry: Arc<SubagentTaskRegistry>,
}

impl DurableSubagentRuntime {
    pub(crate) fn new(
        db: Arc<DbPool>,
        llm: Arc<dyn LlmClient>,
        model: String,
        parent_frame_id: String,
        workspace: String,
        registry: Arc<SubagentTaskRegistry>,
    ) -> Self {
        Self {
            db,
            llm,
            model,
            parent_frame_id,
            workspace,
            registry,
        }
    }

    async fn db<T, F>(&self, f: F) -> Result<T, String>
    where
        T: Send + 'static,
        F: FnOnce(&rusqlite::Connection) -> Result<T, DbError> + Send + 'static,
    {
        crate::db::with_conn(&self.db, f)
            .await
            .map_err(|error| error.to_string())
    }

    async fn accept_child_run(
        &self,
        frame_id: &str,
        task: &str,
        messages: Vec<ChatMessage>,
    ) -> Result<(String, dss_db::repo::PersistAttemptLease), String> {
        let run_id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
        let expires =
            (Utc::now() + chrono::Duration::hours(24)).to_rfc3339_opts(SecondsFormat::Millis, true);
        let request = AcceptFrameRun {
            run_id: run_id.clone(),
            frame_id: frame_id.to_owned(),
            task_summary: task.to_owned(),
            trigger_kind: "delegate".into(),
            started_at: now,
            lease_owner: format!("dss-api:{}", std::process::id()),
            lease_expires_at: expires,
            messages: messages
                .into_iter()
                .map(|message| FrameRunMessage {
                    role: message.role.clone(),
                    content: serde_json::to_value(message).expect("ChatMessage serializes"),
                    harness_notice: false,
                })
                .collect(),
        };
        let attempt = self
            .db(move |conn| accept_frame_run(conn, &request))
            .await?;
        Ok((run_id, attempt))
    }

    async fn spawn_child(
        &self,
        frame_id: String,
        run_id: String,
        attempt: dss_db::repo::PersistAttemptLease,
    ) -> oneshot::Receiver<Result<(), String>> {
        let db = self.db.clone();
        let llm = self.llm.clone();
        let model = self.model.clone();
        let registry = self.registry.clone();
        let (done_tx, done_rx) = oneshot::channel();
        let task_frame_id = frame_id.clone();
        let handle = tokio::spawn(async move {
            let history_frame = task_frame_id.clone();
            let loaded =
                crate::db::with_conn(&db, move |conn| list_frame_messages(conn, &history_frame))
                    .await;
            let outcome = match loaded {
                Ok(values) => {
                    let parsed = values
                        .into_iter()
                        .map(serde_json::from_value::<ChatMessage>)
                        .collect::<Result<Vec<_>, _>>();
                    match parsed {
                        Ok(messages) => llm
                            .chat(ChatRequest::new(model, messages))
                            .await
                            .map(|response| {
                                if response.text.is_empty() {
                                    "(sub-agent returned empty response)".to_owned()
                                } else {
                                    response.text
                                }
                            })
                            .map_err(|error| error.to_string()),
                        Err(error) => Err(format!("invalid durable child transcript: {error}")),
                    }
                }
                Err(error) => Err(error.to_string()),
            };

            let (status, response, payload) = match outcome {
                Ok(text) => {
                    let message = ChatMessage::assistant(text.clone());
                    (
                        "completed",
                        Some(FrameRunMessage {
                            role: message.role.clone(),
                            content: serde_json::to_value(message).expect("ChatMessage serializes"),
                            harness_notice: false,
                        }),
                        json!({"output": text}),
                    )
                }
                Err(error) => ("failed", None, json!({"error": error})),
            };
            let settle_run_id = run_id.clone();
            let settle_frame_id = task_frame_id.clone();
            let settle_attempt = attempt.clone();
            let settle_payload = payload.clone();
            let settled = crate::db::with_conn(&db, move |conn| {
                settle_frame_run(
                    conn,
                    &settle_run_id,
                    &settle_frame_id,
                    &settle_attempt,
                    status,
                    response.as_ref(),
                    &settle_payload,
                )
            })
            .await
            .map_err(|error| error.to_string());
            registry.tasks.lock().await.remove(&task_frame_id);
            let _ = done_tx.send(settled);
        });
        self.registry
            .tasks
            .lock()
            .await
            .insert(frame_id, handle.abort_handle());
        done_rx
    }

    async fn collect_now(&self, frame_ids: &[String]) -> Result<Vec<Value>, String> {
        let mut collected = Vec::new();
        for frame_id in frame_ids {
            let parent = self.parent_frame_id.clone();
            let child = frame_id.clone();
            let rows = self
                .db(move |conn| collect_child_results(conn, &parent, &child))
                .await?;
            collected.extend(rows.into_iter().map(|row| {
                json!({
                    "result_id": row.id,
                    "frame_id": row.child_frame_id,
                    "run_id": row.run_id,
                    "status": row.status,
                    "result": row.payload,
                })
            }));
        }
        Ok(collected)
    }

    async fn ensure_direct_child(
        &self,
        frame_id: &str,
    ) -> Result<dss_db::harness::ExecutionFrameRow, String> {
        let id = frame_id.to_owned();
        let frame = self
            .db(move |conn| get_frame(conn, &id))
            .await?
            .ok_or_else(|| format!("child Frame {frame_id} not found"))?;
        if frame.parent_frame_id.as_deref() != Some(&self.parent_frame_id) {
            return Err(format!(
                "Frame {frame_id} is not a direct child of this agent"
            ));
        }
        Ok(frame)
    }
}

#[async_trait]
impl SubagentRuntime for DurableSubagentRuntime {
    async fn delegate(
        &self,
        task: &str,
        context_summary: Option<&str>,
        wait: bool,
    ) -> Result<Value, String> {
        let frame_id = Uuid::new_v4().to_string();
        let child = NewChildFrame {
            id: frame_id.clone(),
            parent_frame_id: self.parent_frame_id.clone(),
            kind: "delegate".into(),
            profile_id: None,
            hidden: false,
            workspace_scope_id: Some(self.workspace.clone()),
        };
        self.db(move |conn| create_child_frame(conn, &child))
            .await?;
        let system = ChatMessage::system(
            "你是一个被委派的持久化子 agent。专注完成给定子任务，给出简洁、结构化、可审计的结果。",
        );
        let prompt = context_summary
            .map(|context| format!("上下文：{context}\n\n子任务：{task}"))
            .unwrap_or_else(|| format!("子任务：{task}"));
        let (run_id, attempt) = self
            .accept_child_run(&frame_id, task, vec![system, ChatMessage::user(prompt)])
            .await?;
        let done = self
            .spawn_child(frame_id.clone(), run_id.clone(), attempt)
            .await;
        if !wait {
            return Ok(
                json!({"frame_id": frame_id, "run_id": run_id, "status": "running", "collect_only": true}),
            );
        }
        done.await
            .map_err(|_| "child execution task ended without settlement".to_owned())??;
        let results = self.collect_now(std::slice::from_ref(&frame_id)).await?;
        Ok(json!({"frame_id": frame_id, "run_id": run_id, "results": results}))
    }

    async fn collect(&self, frame_ids: &[String], timeout_seconds: u64) -> Result<Value, String> {
        for frame_id in frame_ids {
            self.ensure_direct_child(frame_id).await?;
        }
        let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_seconds);
        loop {
            let results = self.collect_now(frame_ids).await?;
            if !results.is_empty() {
                return Ok(json!({"results": results, "timed_out": false}));
            }
            if tokio::time::Instant::now() >= deadline {
                return Ok(json!({"results": [], "timed_out": true}));
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    async fn send_message(&self, frame_id: &str, message: &str) -> Result<Value, String> {
        let frame = self.ensure_direct_child(frame_id).await?;
        if frame.active_run_id.is_some() {
            return Err(format!(
                "child Frame {frame_id} already has an active Run; wait for it to land or stop it before sending another message"
            ));
        }
        if frame.activity == "closed" {
            return Err("closed child Frame cannot be resumed".into());
        }
        let (run_id, attempt) = self
            .accept_child_run(frame_id, message, vec![ChatMessage::user(message)])
            .await?;
        self.spawn_child(frame_id.to_owned(), run_id.clone(), attempt)
            .await;
        Ok(json!({"frame_id": frame_id, "run_id": run_id, "status": "running"}))
    }

    async fn stop_child(&self, frame_id: &str) -> Result<Value, String> {
        self.ensure_direct_child(frame_id).await?;
        let child = frame_id.to_owned();
        let cancelled = self
            .db(move |conn| cancel_active_frame(conn, &child))
            .await?;
        if cancelled {
            if let Some(handle) = self.registry.tasks.lock().await.remove(frame_id) {
                handle.abort();
            }
        }
        Ok(json!({"frame_id": frame_id, "cancelled": cancelled}))
    }

    async fn children(&self) -> Result<Value, String> {
        let root = self.parent_frame_id.clone();
        let parent = root.clone();
        let frames = self.db(move |conn| list_frame_tree(conn, &root)).await?;
        Ok(
            json!({"children": frames.into_iter().filter(|frame| frame.parent_frame_id.as_deref() == Some(parent.as_str())).collect::<Vec<_>>() }),
        )
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use dss_llm::{BoxedEventStream, LlmError, LlmResponse};

    use super::*;

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("dss-subagent-{}", Uuid::new_v4()));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    struct FakeLlm(AtomicUsize);

    #[async_trait]
    impl LlmClient for FakeLlm {
        async fn chat(&self, request: ChatRequest) -> Result<LlmResponse, LlmError> {
            let call = self.0.fetch_add(1, Ordering::SeqCst) + 1;
            let prompt = request
                .messages
                .iter()
                .rev()
                .find_map(|message| message.content.as_deref())
                .unwrap_or_default();
            Ok(LlmResponse {
                text: format!("child-{call}: {prompt}"),
                ..LlmResponse::default()
            })
        }

        fn chat_stream(
            &self,
            _request: ChatRequest,
        ) -> futures::future::BoxFuture<'_, Result<BoxedEventStream, LlmError>> {
            Box::pin(async { Ok(Box::pin(futures::stream::empty()) as BoxedEventStream) })
        }

        fn model(&self) -> &str {
            "fake"
        }
    }

    #[tokio::test]
    async fn durable_child_lands_collects_and_resumes_on_the_same_frame() {
        let dir = TestDir::new();
        let pool = Arc::new(dss_db::open_pool(&dir.0).unwrap());
        dss_db::run_migrations(&pool).await.unwrap();
        crate::db::with_conn(&pool, |conn| {
            dss_db::repo::create_session(conn, "root", "/tmp/root", None, None)
        })
        .await
        .unwrap();
        let runtime = DurableSubagentRuntime::new(
            pool.clone(),
            Arc::new(FakeLlm(AtomicUsize::new(0))),
            "fake".into(),
            "root".into(),
            "/tmp/root".into(),
            Arc::new(SubagentTaskRegistry::default()),
        );

        let delegated = runtime.delegate("analyze", None, true).await.unwrap();
        let frame_id = delegated["frame_id"].as_str().unwrap().to_owned();
        assert_eq!(delegated["results"][0]["status"], "completed");
        assert!(runtime
            .collect(std::slice::from_ref(&frame_id), 0)
            .await
            .unwrap()["results"]
            .as_array()
            .unwrap()
            .is_empty());

        let resumed = runtime.send_message(&frame_id, "refine").await.unwrap();
        assert_eq!(resumed["frame_id"], frame_id);
        let collected = runtime.collect(&[frame_id.clone()], 5).await.unwrap();
        assert_eq!(collected["results"][0]["frame_id"], frame_id);
        let frames = runtime.children().await.unwrap();
        assert_eq!(frames["children"].as_array().unwrap().len(), 1);

        let counts = crate::db::with_conn(&pool, move |conn| {
            let runs: i64 = conn.query_row(
                "SELECT COUNT(*) FROM session_runs WHERE actor_frame_id=?1",
                [&frame_id],
                |row| row.get(0),
            )?;
            let frames: i64 = conn.query_row(
                "SELECT COUNT(*) FROM execution_frames WHERE id=?1",
                [&frame_id],
                |row| row.get(0),
            )?;
            Ok((runs, frames))
        })
        .await
        .unwrap();
        assert_eq!(counts, (2, 1));
    }

    #[tokio::test]
    async fn active_child_rejects_a_message_it_cannot_consume() {
        let dir = TestDir::new();
        let pool = Arc::new(dss_db::open_pool(&dir.0).unwrap());
        dss_db::run_migrations(&pool).await.unwrap();
        crate::db::with_conn(&pool, |conn| {
            dss_db::repo::create_session(conn, "root", "/tmp/root", None, None)
        })
        .await
        .unwrap();
        let runtime = DurableSubagentRuntime::new(
            pool,
            Arc::new(FakeLlm(AtomicUsize::new(0))),
            "fake".into(),
            "root".into(),
            "/tmp/root".into(),
            Arc::new(SubagentTaskRegistry::default()),
        );
        let child = NewChildFrame {
            id: "busy-child".into(),
            parent_frame_id: "root".into(),
            kind: "delegate".into(),
            profile_id: None,
            hidden: false,
            workspace_scope_id: None,
        };
        runtime
            .db(move |conn| create_child_frame(conn, &child))
            .await
            .unwrap();
        runtime
            .accept_child_run(
                "busy-child",
                "first task",
                vec![ChatMessage::user("first task")],
            )
            .await
            .unwrap();

        let error = runtime
            .send_message("busy-child", "interrupt")
            .await
            .unwrap_err();
        assert!(error.contains("active Run"));
        let unread = runtime
            .db(|conn| dss_db::harness::list_unread_mailbox(conn, "busy-child"))
            .await
            .unwrap();
        assert!(unread.is_empty());
    }
}
