//! Canonical, append-only session event log.
//!
//! Existing projection tables (`session_messages`, `session_runs`, `sessions`) remain in place
//! during the incremental migration. New writes are dual-recorded here so recovery, audit and
//! future projections can converge on one ordered source of durable facts.

use chrono::Utc;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::DbError;

pub const SESSION_EVENT_SCHEMA_VERSION: i64 = 1;
pub const MAX_EVENT_PAGE_SIZE: usize = 1_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionEventKind {
    SessionCreated,
    FrameCreated,
    FrameActivityChanged,
    RunAccepted,
    RunWaiting,
    RunResumed,
    RunInterrupted,
    AttemptStarted,
    AttemptCheckpointed,
    AttemptLeaseExpired,
    AttemptSettled,
    ToolCallStarted,
    ToolCallSettled,
    MessageAppended,
    RunCheckpointed,
    RunCompleted,
    CompactionUpdated,
    PlanUpdated,
    JobEnqueued,
    JobClaimed,
    JobSettled,
    InputRequested,
    InputResolved,
    ChildLanded,
    ChildResultCollected,
    FrameClosed,
    ToolReconciliationRequired,
    LegacyFrameBaselineAttached,
}

impl SessionEventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SessionCreated => "session_created",
            Self::FrameCreated => "frame_created",
            Self::FrameActivityChanged => "frame_activity_changed",
            Self::RunAccepted => "run_accepted",
            Self::RunWaiting => "run_waiting",
            Self::RunResumed => "run_resumed",
            Self::RunInterrupted => "run_interrupted",
            Self::AttemptStarted => "attempt_started",
            Self::AttemptCheckpointed => "attempt_checkpointed",
            Self::AttemptLeaseExpired => "attempt_lease_expired",
            Self::AttemptSettled => "attempt_settled",
            Self::ToolCallStarted => "tool_call_started",
            Self::ToolCallSettled => "tool_call_settled",
            Self::MessageAppended => "message_appended",
            Self::RunCheckpointed => "run_checkpointed",
            Self::RunCompleted => "run_completed",
            Self::CompactionUpdated => "compaction_updated",
            Self::PlanUpdated => "plan_updated",
            Self::JobEnqueued => "job_enqueued",
            Self::JobClaimed => "job_claimed",
            Self::JobSettled => "job_settled",
            Self::InputRequested => "input_requested",
            Self::InputResolved => "input_resolved",
            Self::ChildLanded => "child_landed",
            Self::ChildResultCollected => "child_result_collected",
            Self::FrameClosed => "frame_closed",
            Self::ToolReconciliationRequired => "tool_reconciliation_required",
            Self::LegacyFrameBaselineAttached => "legacy_frame_baseline_attached",
        }
    }

    fn parse(value: &str) -> Result<Self, DbError> {
        match value {
            "session_created" => Ok(Self::SessionCreated),
            "frame_created" => Ok(Self::FrameCreated),
            "frame_activity_changed" => Ok(Self::FrameActivityChanged),
            "run_accepted" => Ok(Self::RunAccepted),
            "run_waiting" => Ok(Self::RunWaiting),
            "run_resumed" => Ok(Self::RunResumed),
            "run_interrupted" => Ok(Self::RunInterrupted),
            "attempt_started" => Ok(Self::AttemptStarted),
            "attempt_checkpointed" => Ok(Self::AttemptCheckpointed),
            "attempt_lease_expired" => Ok(Self::AttemptLeaseExpired),
            "attempt_settled" => Ok(Self::AttemptSettled),
            "tool_call_started" => Ok(Self::ToolCallStarted),
            "tool_call_settled" => Ok(Self::ToolCallSettled),
            "message_appended" => Ok(Self::MessageAppended),
            "run_checkpointed" => Ok(Self::RunCheckpointed),
            "run_completed" => Ok(Self::RunCompleted),
            "compaction_updated" => Ok(Self::CompactionUpdated),
            "plan_updated" => Ok(Self::PlanUpdated),
            "job_enqueued" => Ok(Self::JobEnqueued),
            "job_claimed" => Ok(Self::JobClaimed),
            "job_settled" => Ok(Self::JobSettled),
            "input_requested" => Ok(Self::InputRequested),
            "input_resolved" => Ok(Self::InputResolved),
            "child_landed" => Ok(Self::ChildLanded),
            "child_result_collected" => Ok(Self::ChildResultCollected),
            "frame_closed" => Ok(Self::FrameClosed),
            "tool_reconciliation_required" => Ok(Self::ToolReconciliationRequired),
            "legacy_frame_baseline_attached" => Ok(Self::LegacyFrameBaselineAttached),
            other => Err(DbError::Other(format!(
                "unknown session event type {other:?}"
            ))),
        }
    }
}

#[derive(Debug, Clone)]
pub struct NewSessionEvent {
    pub session_id: String,
    pub run_id: Option<String>,
    pub frame_id: Option<String>,
    pub kind: SessionEventKind,
    pub payload: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionEventRow {
    pub event_id: String,
    pub session_id: String,
    pub seq: i64,
    pub run_id: Option<String>,
    pub frame_id: Option<String>,
    pub kind: SessionEventKind,
    pub schema_version: i64,
    pub payload: Value,
    pub created_at: String,
}

fn now() -> String {
    Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// Append an event using the caller's connection or transaction.
///
/// SQLite serializes writers; callers that need an event and a projection update to be atomic
/// must invoke this helper from the same transaction as that projection update.
pub(crate) fn append_event_in_transaction(
    conn: &Connection,
    event: &NewSessionEvent,
    created_at: &str,
) -> Result<SessionEventRow, DbError> {
    let event_id = Uuid::new_v4().to_string();
    let payload = serde_json::to_string(&event.payload)
        .map_err(|error| DbError::Other(format!("serialize session event: {error}")))?;
    // Allocate the sequence inside the INSERT statement. A separate SELECT followed by INSERT
    // lets two deferred SQLite transactions observe the same MAX(seq), which used to make a
    // concurrent job claim and run acceptance race into SQLITE_BUSY/UNIQUE and surface as HTTP
    // 500. The write statement is serialized before its subquery is evaluated.
    conn.execute(
        "INSERT INTO session_events (\
             event_id, session_id, seq, run_id, frame_id, event_type, schema_version, payload, created_at\
         ) SELECT ?1, ?2, COALESCE(MAX(seq), 0) + 1, ?3, ?4, ?5, ?6, ?7, ?8 \
           FROM session_events WHERE session_id = ?2",
        params![
            event_id,
            event.session_id,
            event.run_id,
            event.frame_id,
            event.kind.as_str(),
            SESSION_EVENT_SCHEMA_VERSION,
            payload,
            created_at,
        ],
    )?;
    let seq: i64 = conn.query_row(
        "SELECT seq FROM session_events WHERE event_id = ?1",
        params![event_id],
        |row| row.get(0),
    )?;
    Ok(SessionEventRow {
        event_id,
        session_id: event.session_id.clone(),
        seq,
        run_id: event.run_id.clone(),
        frame_id: event.frame_id.clone(),
        kind: event.kind,
        schema_version: SESSION_EVENT_SCHEMA_VERSION,
        payload: event.payload.clone(),
        created_at: created_at.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier};

    use rusqlite::Connection;
    use serde_json::json;

    use super::{append_session_event, NewSessionEvent, SessionEventKind};

    #[test]
    fn concurrent_event_appends_keep_one_contiguous_sequence() {
        const WRITERS: usize = 4;
        const EVENTS_PER_WRITER: usize = 25;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.db");
        let setup = Connection::open(&path).unwrap();
        setup
            .execute_batch(
                "PRAGMA journal_mode=WAL;\
                 PRAGMA busy_timeout=5000;\
                 CREATE TABLE sessions (id TEXT PRIMARY KEY);\
                 CREATE TABLE session_events (\
                   event_id TEXT PRIMARY KEY, session_id TEXT NOT NULL REFERENCES sessions(id),\
                   seq INTEGER NOT NULL, run_id TEXT, frame_id TEXT, event_type TEXT NOT NULL,\
                   schema_version INTEGER NOT NULL, payload TEXT NOT NULL, created_at TEXT NOT NULL,\
                   UNIQUE(session_id, seq));\
                 INSERT INTO sessions (id) VALUES ('session-race');",
            )
            .unwrap();
        drop(setup);

        let barrier = Arc::new(Barrier::new(WRITERS));
        let handles = (0..WRITERS)
            .map(|writer| {
                let path = path.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    let conn = Connection::open(path).unwrap();
                    conn.pragma_update(None, "busy_timeout", 5000i64).unwrap();
                    barrier.wait();
                    for index in 0..EVENTS_PER_WRITER {
                        append_session_event(
                            &conn,
                            &NewSessionEvent {
                                session_id: "session-race".into(),
                                run_id: Some(format!("run-{writer}")),
                                frame_id: None,
                                kind: SessionEventKind::RunAccepted,
                                payload: json!({ "writer": writer, "index": index }),
                            },
                        )
                        .unwrap();
                    }
                })
            })
            .collect::<Vec<_>>();
        for handle in handles {
            handle.join().unwrap();
        }

        let conn = Connection::open(path).unwrap();
        let sequences = conn
            .prepare("SELECT seq FROM session_events WHERE session_id='session-race' ORDER BY seq")
            .unwrap()
            .query_map([], |row| row.get::<_, i64>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(sequences.len(), WRITERS * EVENTS_PER_WRITER);
        assert_eq!(
            sequences,
            (1..=(WRITERS * EVENTS_PER_WRITER) as i64).collect::<Vec<_>>()
        );
    }
}

pub fn append_session_event(
    conn: &Connection,
    event: &NewSessionEvent,
) -> Result<SessionEventRow, DbError> {
    let tx = conn.unchecked_transaction()?;
    let row = append_event_in_transaction(&tx, event, &now())?;
    tx.commit()?;
    Ok(row)
}

pub fn list_session_events(
    conn: &Connection,
    session_id: &str,
    after_seq: i64,
    limit: usize,
) -> Result<Vec<SessionEventRow>, DbError> {
    let limit = limit.clamp(1, MAX_EVENT_PAGE_SIZE) as i64;
    let mut stmt = conn.prepare(
        "SELECT event_id, session_id, seq, run_id, frame_id, event_type, \
                schema_version, payload, created_at \
         FROM session_events WHERE session_id = ?1 AND seq > ?2 \
         ORDER BY seq ASC LIMIT ?3",
    )?;
    let rows = stmt
        .query_map(params![session_id, after_seq.max(0), limit], |row| {
            let kind = row.get::<_, String>(5)?;
            let payload = row.get::<_, String>(7)?;
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                kind,
                row.get::<_, i64>(6)?,
                payload,
                row.get::<_, String>(8)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;

    rows.into_iter()
        .map(
            |(
                event_id,
                session_id,
                seq,
                run_id,
                frame_id,
                kind,
                schema_version,
                payload,
                created_at,
            )| {
                Ok(SessionEventRow {
                    event_id,
                    session_id,
                    seq,
                    run_id,
                    frame_id,
                    kind: SessionEventKind::parse(&kind)?,
                    schema_version,
                    payload: serde_json::from_str(&payload).map_err(|error| {
                        DbError::Other(format!("decode session event {seq}: {error}"))
                    })?,
                    created_at,
                })
            },
        )
        .collect()
}
