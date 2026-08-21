//! Durable Frame topology, execution attempts, and parent/child delivery.

use chrono::{Duration, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::events::{append_event_in_transaction, NewSessionEvent, SessionEventKind};
use crate::repo::PersistAttemptLease;
use crate::DbError;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExecutionFrameRow {
    pub id: String,
    pub session_id: String,
    pub parent_frame_id: Option<String>,
    pub root_frame_id: String,
    pub kind: String,
    pub profile_id: Option<String>,
    pub visibility: String,
    pub activity: String,
    pub active_run_id: Option<String>,
    pub workspace_scope_id: Option<String>,
    pub revision: i64,
    pub created_at: String,
    pub updated_at: String,
    pub closed_at: Option<String>,
}

#[derive(Debug, Clone)]
pub struct NewChildFrame {
    pub id: String,
    pub parent_frame_id: String,
    pub kind: String,
    pub profile_id: Option<String>,
    pub hidden: bool,
    pub workspace_scope_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunAttemptRow {
    pub attempt_id: String,
    pub run_id: String,
    pub attempt_no: i64,
    pub lease_owner: String,
    pub lease_token: String,
    pub lease_expires_at: String,
    pub checkpoint_event_seq: Option<i64>,
    pub status: String,
    pub error: Option<String>,
    pub started_at: String,
    pub ended_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MailboxRow {
    pub id: String,
    pub sender_frame_id: Option<String>,
    pub recipient_frame_id: String,
    pub message_kind: String,
    pub payload: Value,
    pub correlation_id: Option<String>,
    pub status: String,
    pub created_at: String,
    pub read_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChildResultRow {
    pub id: String,
    pub parent_frame_id: String,
    pub child_frame_id: String,
    pub run_id: String,
    pub status: String,
    pub payload: Value,
    pub created_at: String,
}

#[derive(Debug, Clone)]
pub struct FrameRunMessage {
    pub role: String,
    pub content: Value,
    pub harness_notice: bool,
}

#[derive(Debug, Clone)]
pub struct AcceptFrameRun {
    pub run_id: String,
    pub frame_id: String,
    pub task_summary: String,
    pub trigger_kind: String,
    pub started_at: String,
    pub lease_owner: String,
    pub lease_expires_at: String,
    pub messages: Vec<FrameRunMessage>,
}

fn now() -> String {
    Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn parse_json(value: String) -> Result<Value, rusqlite::Error> {
    serde_json::from_str(&value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            value.len(),
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })
}

fn encode_json(value: &Value) -> Result<String, DbError> {
    serde_json::to_string(value)
        .map_err(|error| DbError::Other(format!("serialize harness payload: {error}")))
}

fn map_frame(row: &rusqlite::Row<'_>) -> rusqlite::Result<ExecutionFrameRow> {
    Ok(ExecutionFrameRow {
        id: row.get(0)?,
        session_id: row.get(1)?,
        parent_frame_id: row.get(2)?,
        root_frame_id: row.get(3)?,
        kind: row.get(4)?,
        profile_id: row.get(5)?,
        visibility: row.get(6)?,
        activity: row.get(7)?,
        active_run_id: row.get(8)?,
        workspace_scope_id: row.get(9)?,
        revision: row.get(10)?,
        created_at: row.get(11)?,
        updated_at: row.get(12)?,
        closed_at: row.get(13)?,
    })
}

const FRAME_COLUMNS: &str = "id, session_id, parent_frame_id, root_frame_id, kind, profile_id, \
    visibility, activity, active_run_id, workspace_scope_id, revision, created_at, updated_at, closed_at";

pub fn get_frame(conn: &Connection, id: &str) -> Result<Option<ExecutionFrameRow>, DbError> {
    conn.query_row(
        &format!("SELECT {FRAME_COLUMNS} FROM execution_frames WHERE id=?1"),
        params![id],
        map_frame,
    )
    .optional()
    .map_err(Into::into)
}

pub fn list_frame_tree(
    conn: &Connection,
    root_id: &str,
) -> Result<Vec<ExecutionFrameRow>, DbError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {FRAME_COLUMNS} FROM execution_frames WHERE root_frame_id=?1 ORDER BY created_at, id"
    ))?;
    let rows = stmt
        .query_map(params![root_id], map_frame)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn create_child_frame(
    conn: &Connection,
    child: &NewChildFrame,
) -> Result<ExecutionFrameRow, DbError> {
    if !matches!(
        child.kind.as_str(),
        "delegate" | "reviewer" | "bookmarker" | "aside"
    ) {
        return Err(DbError::Other(format!(
            "invalid child frame kind {:?}",
            child.kind
        )));
    }
    let tx = conn.unchecked_transaction()?;
    let parent = tx
        .query_row(
            &format!("SELECT {FRAME_COLUMNS} FROM execution_frames WHERE id=?1"),
            params![child.parent_frame_id],
            map_frame,
        )
        .optional()?
        .ok_or_else(|| DbError::NotFound(format!("frame {}", child.parent_frame_id)))?;
    if parent.activity == "closed" {
        return Err(DbError::Conflict(
            "cannot attach a child to a closed frame".into(),
        ));
    }
    let timestamp = now();
    tx.execute(
        "INSERT INTO execution_frames (id, session_id, parent_frame_id, root_frame_id, kind, \
             profile_id, visibility, activity, workspace_scope_id, revision, created_at, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'idle', ?8, 1, ?9, ?9)",
        params![
            child.id,
            parent.session_id,
            parent.id,
            parent.root_frame_id,
            child.kind,
            child.profile_id,
            if child.hidden { "hidden" } else { "normal" },
            child.workspace_scope_id,
            timestamp,
        ],
    )?;
    append_event_in_transaction(
        &tx,
        &NewSessionEvent {
            session_id: parent.session_id.clone(),
            run_id: None,
            frame_id: Some(child.id.clone()),
            kind: SessionEventKind::FrameCreated,
            payload: json!({
                "parent_frame_id": parent.id,
                "root_frame_id": parent.root_frame_id,
                "kind": child.kind,
                "visibility": if child.hidden { "hidden" } else { "normal" },
            }),
        },
        &timestamp,
    )?;
    tx.commit()?;
    get_frame(conn, &child.id)?.ok_or_else(|| DbError::Other("created frame missing".into()))
}

pub fn close_frame(conn: &Connection, id: &str, expected_revision: i64) -> Result<(), DbError> {
    let timestamp = now();
    let tx = conn.unchecked_transaction()?;
    let frame = tx
        .query_row(
            &format!("SELECT {FRAME_COLUMNS} FROM execution_frames WHERE id=?1"),
            params![id],
            map_frame,
        )
        .optional()?
        .ok_or_else(|| DbError::NotFound(format!("frame {id}")))?;
    if frame.active_run_id.is_some() {
        return Err(DbError::Conflict(
            "cannot close a frame with an active run".into(),
        ));
    }
    let changed = tx.execute(
        "UPDATE execution_frames SET activity='closed', revision=revision+1, updated_at=?1, \
             closed_at=?1 WHERE id=?2 AND revision=?3 AND activity<>'closed'",
        params![timestamp, id, expected_revision],
    )?;
    if changed != 1 {
        return Err(DbError::Conflict(
            "frame revision changed while closing".into(),
        ));
    }
    append_event_in_transaction(
        &tx,
        &NewSessionEvent {
            session_id: frame.session_id,
            run_id: None,
            frame_id: Some(id.to_string()),
            kind: SessionEventKind::FrameClosed,
            payload: json!({"previous_revision": expected_revision}),
        },
        &timestamp,
    )?;
    tx.commit()?;
    Ok(())
}

pub fn post_mailbox_message(
    conn: &Connection,
    sender_id: &str,
    recipient_id: &str,
    kind: &str,
    payload: &Value,
    correlation_id: Option<&str>,
) -> Result<MailboxRow, DbError> {
    let sender = get_frame(conn, sender_id)?
        .ok_or_else(|| DbError::NotFound(format!("frame {sender_id}")))?;
    let recipient = get_frame(conn, recipient_id)?
        .ok_or_else(|| DbError::NotFound(format!("frame {recipient_id}")))?;
    let directly_related = sender.parent_frame_id.as_deref() == Some(recipient_id)
        || recipient.parent_frame_id.as_deref() == Some(sender_id);
    if sender.session_id != recipient.session_id || !directly_related {
        return Err(DbError::Conflict(
            "frame messages are limited to a direct parent/child edge".into(),
        ));
    }
    let id = Uuid::new_v4().to_string();
    let timestamp = now();
    conn.execute(
        "INSERT INTO frame_mailbox (id, session_id, sender_frame_id, recipient_frame_id, \
             message_kind, payload, correlation_id, status, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'unread', ?8)",
        params![
            id,
            sender.session_id,
            sender_id,
            recipient_id,
            kind,
            encode_json(payload)?,
            correlation_id,
            timestamp,
        ],
    )?;
    Ok(MailboxRow {
        id,
        sender_frame_id: Some(sender_id.into()),
        recipient_frame_id: recipient_id.into(),
        message_kind: kind.into(),
        payload: payload.clone(),
        correlation_id: correlation_id.map(str::to_owned),
        status: "unread".into(),
        created_at: timestamp,
        read_at: None,
    })
}

pub fn list_unread_mailbox(conn: &Connection, frame_id: &str) -> Result<Vec<MailboxRow>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT id, sender_frame_id, recipient_frame_id, message_kind, payload, correlation_id, \
                status, created_at, read_at FROM frame_mailbox \
         WHERE recipient_frame_id=?1 AND status='unread' ORDER BY created_at, id",
    )?;
    let rows = stmt
        .query_map(params![frame_id], |row| {
            Ok(MailboxRow {
                id: row.get(0)?,
                sender_frame_id: row.get(1)?,
                recipient_frame_id: row.get(2)?,
                message_kind: row.get(3)?,
                payload: parse_json(row.get(4)?)?,
                correlation_id: row.get(5)?,
                status: row.get(6)?,
                created_at: row.get(7)?,
                read_at: row.get(8)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn land_child_result(
    conn: &Connection,
    child_frame_id: &str,
    run_id: &str,
    status: &str,
    payload: &Value,
) -> Result<ChildResultRow, DbError> {
    let tx = conn.unchecked_transaction()?;
    let child = tx
        .query_row(
            &format!("SELECT {FRAME_COLUMNS} FROM execution_frames WHERE id=?1"),
            params![child_frame_id],
            map_frame,
        )
        .optional()?
        .ok_or_else(|| DbError::NotFound(format!("frame {child_frame_id}")))?;
    let parent_id = child
        .parent_frame_id
        .clone()
        .ok_or_else(|| DbError::Conflict("root frames cannot land a child result".into()))?;
    let timestamp = now();
    let result_id = Uuid::new_v4().to_string();
    tx.execute(
        "INSERT INTO child_results (id, parent_frame_id, child_frame_id, run_id, status, payload, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![result_id, parent_id, child_frame_id, run_id, status, encode_json(payload)?, timestamp],
    )?;
    let wake_id = Uuid::new_v4().to_string();
    tx.execute(
        "INSERT INTO frame_mailbox (id, session_id, sender_frame_id, recipient_frame_id, \
             message_kind, payload, correlation_id, status, created_at) \
         VALUES (?1, ?2, ?3, ?4, 'child_landed', ?5, ?6, 'unread', ?7)",
        params![
            wake_id,
            child.session_id,
            child_frame_id,
            parent_id,
            encode_json(&json!({"child_frame_id": child_frame_id, "run_id": run_id}))?,
            result_id,
            timestamp,
        ],
    )?;
    append_event_in_transaction(
        &tx,
        &NewSessionEvent {
            session_id: child.session_id,
            run_id: Some(run_id.into()),
            frame_id: Some(child_frame_id.into()),
            kind: SessionEventKind::ChildLanded,
            payload: json!({"parent_frame_id": parent_id, "result_id": result_id, "status": status}),
        },
        &timestamp,
    )?;
    tx.commit()?;
    Ok(ChildResultRow {
        id: result_id,
        parent_frame_id: parent_id,
        child_frame_id: child_frame_id.into(),
        run_id: run_id.into(),
        status: status.into(),
        payload: payload.clone(),
        created_at: timestamp,
    })
}

pub fn collect_child_results(
    conn: &Connection,
    collector_frame_id: &str,
    child_frame_id: &str,
) -> Result<Vec<ChildResultRow>, DbError> {
    let collector = get_frame(conn, collector_frame_id)?
        .ok_or_else(|| DbError::NotFound(format!("frame {collector_frame_id}")))?;
    let child = get_frame(conn, child_frame_id)?
        .ok_or_else(|| DbError::NotFound(format!("frame {child_frame_id}")))?;
    if child.parent_frame_id.as_deref() != Some(collector_frame_id) {
        return Err(DbError::Conflict(
            "only a direct parent may collect child results".into(),
        ));
    }
    let timestamp = now();
    let tx = conn.unchecked_transaction()?;
    let mut stmt = tx.prepare(
        "SELECT r.id, r.parent_frame_id, r.child_frame_id, r.run_id, r.status, r.payload, r.created_at \
         FROM child_results r LEFT JOIN child_result_collections c \
           ON c.result_id=r.id AND c.collector_frame_id=?1 \
         WHERE r.child_frame_id=?2 AND c.result_id IS NULL ORDER BY r.created_at, r.id",
    )?;
    let results = stmt
        .query_map(params![collector_frame_id, child_frame_id], |row| {
            Ok(ChildResultRow {
                id: row.get(0)?,
                parent_frame_id: row.get(1)?,
                child_frame_id: row.get(2)?,
                run_id: row.get(3)?,
                status: row.get(4)?,
                payload: parse_json(row.get(5)?)?,
                created_at: row.get(6)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(stmt);
    for result in &results {
        tx.execute(
            "INSERT INTO child_result_collections (result_id, collector_frame_id, collected_at) VALUES (?1, ?2, ?3)",
            params![result.id, collector_frame_id, timestamp],
        )?;
        append_event_in_transaction(
            &tx,
            &NewSessionEvent {
                session_id: collector.session_id.clone(),
                run_id: Some(result.run_id.clone()),
                frame_id: Some(collector_frame_id.into()),
                kind: SessionEventKind::ChildResultCollected,
                payload: json!({"result_id": result.id, "child_frame_id": child_frame_id}),
            },
            &timestamp,
        )?;
    }
    tx.execute(
        "UPDATE frame_mailbox SET status='read', read_at=?1 WHERE recipient_frame_id=?2 \
         AND sender_frame_id=?3 AND message_kind='child_landed' AND status='unread'",
        params![timestamp, collector_frame_id, child_frame_id],
    )?;
    tx.commit()?;
    Ok(results)
}

pub fn start_attempt(
    conn: &Connection,
    run_id: &str,
    lease_owner: &str,
    lease_seconds: i64,
) -> Result<RunAttemptRow, DbError> {
    let timestamp = now();
    let expiry = (Utc::now() + Duration::seconds(lease_seconds.max(1)))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let attempt_id = Uuid::new_v4().to_string();
    let lease_token = Uuid::new_v4().to_string();
    let tx = conn.unchecked_transaction()?;
    let (session_id, frame_id, attempt_no): (String, String, i64) = tx
        .query_row(
            "SELECT session_id, actor_frame_id, \
                 COALESCE((SELECT MAX(attempt_no) FROM run_attempts WHERE run_id=?1), 0) + 1 \
             FROM session_runs WHERE run_id=?1",
            params![run_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?
        .ok_or_else(|| DbError::NotFound(format!("run {run_id}")))?;
    tx.execute(
        "INSERT INTO run_attempts (attempt_id, run_id, attempt_no, lease_owner, lease_token, \
             lease_expires_at, status, started_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'running', ?7)",
        params![
            attempt_id,
            run_id,
            attempt_no,
            lease_owner,
            lease_token,
            expiry,
            timestamp
        ],
    )?;
    let changed = tx.execute(
        "UPDATE session_runs SET active_attempt_id=?1, status='processing' \
         WHERE run_id=?2 AND (active_attempt_id IS NULL OR active_attempt_id NOT IN \
             (SELECT attempt_id FROM run_attempts WHERE status IN ('running','waiting')))",
        params![attempt_id, run_id],
    )?;
    if changed != 1 {
        return Err(DbError::Conflict("run already has a live attempt".into()));
    }
    tx.execute(
        "UPDATE execution_frames SET active_run_id=?1, activity='running', revision=revision+1, updated_at=?2 \
         WHERE id=?3 AND activity<>'closed' AND (active_run_id IS NULL OR active_run_id=?1)",
        params![run_id, timestamp, frame_id],
    )?;
    append_event_in_transaction(
        &tx,
        &NewSessionEvent {
            session_id,
            run_id: Some(run_id.into()),
            frame_id: Some(frame_id),
            kind: SessionEventKind::AttemptStarted,
            payload: json!({"attempt_id": attempt_id, "attempt_no": attempt_no, "lease_owner": lease_owner, "lease_expires_at": expiry}),
        },
        &timestamp,
    )?;
    tx.commit()?;
    Ok(RunAttemptRow {
        attempt_id,
        run_id: run_id.into(),
        attempt_no,
        lease_owner: lease_owner.into(),
        lease_token,
        lease_expires_at: expiry,
        checkpoint_event_seq: None,
        status: "running".into(),
        error: None,
        started_at: timestamp,
        ended_at: None,
    })
}

#[derive(Debug, Clone)]
pub struct ToolCallStart<'a> {
    pub call_id: &'a str,
    pub run_id: &'a str,
    pub attempt_id: &'a str,
    pub lease_token: &'a str,
    pub tool_name: &'a str,
    pub effect_class: &'a str,
    pub input: &'a Value,
    pub idempotency_key: Option<&'a str>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UnresolvedToolCallRow {
    /// Provider-local call id, without the durable `{run_id}:` namespace prefix.
    pub call_id: String,
    pub run_id: String,
    pub attempt_id: Option<String>,
    pub tool_name: String,
    pub effect_class: String,
    pub status: String,
    pub input: Value,
    pub detail: Option<Value>,
    pub started_at: String,
}

pub fn record_tool_call_started(
    conn: &Connection,
    call: &ToolCallStart<'_>,
) -> Result<(), DbError> {
    if !matches!(
        call.effect_class,
        "read_only" | "idempotent" | "external_side_effect"
    ) {
        return Err(DbError::Other(format!(
            "invalid tool effect class {}",
            call.effect_class
        )));
    }
    let tx = conn.unchecked_transaction()?;
    let owner: Option<(String, String)> = tx
        .query_row(
            "SELECT r.session_id, r.actor_frame_id FROM run_attempts a \
             JOIN session_runs r ON r.run_id=a.run_id \
             WHERE a.attempt_id=?1 AND a.run_id=?2 AND a.lease_token=?3 AND a.status='running' \
               AND r.active_attempt_id=a.attempt_id",
            params![call.attempt_id, call.run_id, call.lease_token],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let (session_id, frame_id) =
        owner.ok_or_else(|| DbError::Conflict("stale attempt cannot start a tool call".into()))?;
    let durable_call_id = format!("{}:{}", call.run_id, call.call_id);
    let timestamp = now();
    tx.execute(
        "INSERT INTO tool_call_attempts (call_id, run_id, attempt_id, tool_name, idempotency_key, \
             effect_class, status, input_json, started_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'started', ?7, ?8)",
        params![
            durable_call_id,
            call.run_id,
            call.attempt_id,
            call.tool_name,
            call.idempotency_key,
            call.effect_class,
            encode_json(call.input)?,
            timestamp,
        ],
    )?;
    append_event_in_transaction(
        &tx,
        &NewSessionEvent {
            session_id,
            run_id: Some(call.run_id.into()),
            frame_id: Some(frame_id),
            kind: SessionEventKind::ToolCallStarted,
            payload: json!({
                "call_id": call.call_id,
                "attempt_id": call.attempt_id,
                "tool_name": call.tool_name,
                "effect_class": call.effect_class,
                "idempotency_key": call.idempotency_key,
            }),
        },
        &timestamp,
    )?;
    tx.commit()?;
    Ok(())
}

pub fn record_tool_call_settled(
    conn: &Connection,
    call_id: &str,
    run_id: &str,
    attempt_id: &str,
    lease_token: &str,
    succeeded: bool,
    output: &Value,
) -> Result<(), DbError> {
    let tx = conn.unchecked_transaction()?;
    let owner: Option<(String, String)> = tx
        .query_row(
            "SELECT r.session_id, r.actor_frame_id FROM run_attempts a \
             JOIN session_runs r ON r.run_id=a.run_id \
             WHERE a.attempt_id=?1 AND a.run_id=?2 AND a.lease_token=?3 AND a.status='running' \
               AND r.active_attempt_id=a.attempt_id",
            params![attempt_id, run_id, lease_token],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let (session_id, frame_id) =
        owner.ok_or_else(|| DbError::Conflict("stale attempt cannot settle a tool call".into()))?;
    let durable_call_id = format!("{run_id}:{call_id}");
    let timestamp = now();
    let changed = tx.execute(
        "UPDATE tool_call_attempts SET status=?1, output_json=?2, settled_at=?3 \
         WHERE call_id=?4 AND run_id=?5 AND attempt_id=?6 AND status='started'",
        params![
            if succeeded { "succeeded" } else { "failed" },
            encode_json(output)?,
            timestamp,
            durable_call_id,
            run_id,
            attempt_id,
        ],
    )?;
    if changed != 1 {
        return Err(DbError::Conflict(
            "tool call was already settled or replaced".into(),
        ));
    }
    append_event_in_transaction(
        &tx,
        &NewSessionEvent {
            session_id,
            run_id: Some(run_id.into()),
            frame_id: Some(frame_id),
            kind: SessionEventKind::ToolCallSettled,
            payload: json!({
                "call_id": call_id,
                "attempt_id": attempt_id,
                "status": if succeeded { "succeeded" } else { "failed" },
            }),
        },
        &timestamp,
    )?;
    tx.commit()?;
    Ok(())
}

/// Mark a tool invocation as having an ambiguous external outcome. The Run is transitioned by
/// its normal terminal persistence transaction after the Runner checkpoints the paired tool
/// trace; until then this `unknown` row is a fail-closed terminal-write guard.
pub fn record_tool_call_uncertain(
    conn: &Connection,
    call_id: &str,
    run_id: &str,
    attempt_id: &str,
    lease_token: &str,
    reason: &str,
    detail: &Value,
) -> Result<(), DbError> {
    let tx = conn.unchecked_transaction()?;
    let owner: Option<(String, String, String)> = tx
        .query_row(
            "SELECT r.session_id, r.actor_frame_id, t.tool_name FROM run_attempts a \
             JOIN session_runs r ON r.run_id=a.run_id \
             JOIN tool_call_attempts t ON t.run_id=r.run_id AND t.attempt_id=a.attempt_id \
             WHERE a.attempt_id=?1 AND a.run_id=?2 AND a.lease_token=?3 AND a.status='running' \
               AND r.active_attempt_id=a.attempt_id AND t.call_id=?4 AND t.status='started' \
               AND t.effect_class='external_side_effect'",
            params![
                attempt_id,
                run_id,
                lease_token,
                format!("{run_id}:{call_id}")
            ],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let (session_id, frame_id, tool_name) = owner.ok_or_else(|| {
        DbError::Conflict("tool call is not an owned external side effect".into())
    })?;
    let timestamp = now();
    let output = json!({"reason": reason, "detail": detail});
    let changed = tx.execute(
        "UPDATE tool_call_attempts SET status='unknown', output_json=?1, settled_at=?2 \
         WHERE call_id=?3 AND attempt_id=?4 AND status='started'",
        params![
            encode_json(&output)?,
            timestamp,
            format!("{run_id}:{call_id}"),
            attempt_id
        ],
    )?;
    if changed != 1 {
        return Err(DbError::Conflict(
            "tool call was already settled or replaced".into(),
        ));
    }
    append_event_in_transaction(
        &tx,
        &NewSessionEvent {
            session_id,
            run_id: Some(run_id.into()),
            frame_id: Some(frame_id),
            kind: SessionEventKind::ToolReconciliationRequired,
            payload: json!({
                "call_id": call_id,
                "attempt_id": attempt_id,
                "tool_name": tool_name,
                "reason": reason,
            }),
        },
        &timestamp,
    )?;
    tx.commit()?;
    Ok(())
}

pub fn list_unresolved_tool_calls(
    conn: &Connection,
    run_id: &str,
) -> Result<Vec<UnresolvedToolCallRow>, DbError> {
    let prefix = format!("{run_id}:");
    let mut stmt = conn.prepare(
        "SELECT call_id, run_id, attempt_id, tool_name, effect_class, status, input_json, \
                output_json, started_at FROM tool_call_attempts \
         WHERE run_id=?1 AND effect_class='external_side_effect' \
           AND status IN ('started','unknown') ORDER BY started_at, call_id",
    )?;
    let rows = stmt
        .query_map(params![run_id], |row| {
            let durable_call_id: String = row.get(0)?;
            let detail = row
                .get::<_, Option<String>>(7)?
                .map(parse_json)
                .transpose()?;
            Ok(UnresolvedToolCallRow {
                call_id: durable_call_id
                    .strip_prefix(&prefix)
                    .unwrap_or(&durable_call_id)
                    .to_owned(),
                run_id: row.get(1)?,
                attempt_id: row.get(2)?,
                tool_name: row.get(3)?,
                effect_class: row.get(4)?,
                status: row.get(5)?,
                input: parse_json(row.get(6)?)?,
                detail,
                started_at: row.get(8)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Resolve a tool call whose external outcome could not be observed before ownership was lost.
/// The supplied outcome becomes a canonical harness notice. It is deliberately not a `tool`
/// message: the crash may have happened before the matching assistant tool-call checkpoint, and
/// synthesizing half of an OpenAI tool transaction would make the resumed transcript invalid.
pub fn resolve_tool_reconciliation(
    conn: &Connection,
    run_id: &str,
    call_id: &str,
    succeeded: bool,
    output: &Value,
) -> Result<bool, DbError> {
    let tx = conn.unchecked_transaction()?;
    let durable_call_id = format!("{run_id}:{call_id}");
    let owner: Option<(String, String, String)> = tx
        .query_row(
            "SELECT r.session_id, r.actor_frame_id, t.tool_name FROM session_runs r \
             JOIN tool_call_attempts t ON t.run_id=r.run_id \
             WHERE r.run_id=?1 AND r.status='needs_reconciliation' AND t.call_id=?2 \
               AND t.status IN ('started','unknown') AND t.effect_class='external_side_effect'",
            params![run_id, durable_call_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let (session_id, frame_id, tool_name) = owner.ok_or_else(|| {
        DbError::Conflict("tool call is not awaiting external reconciliation".into())
    })?;
    let timestamp = now();
    tx.execute(
        "UPDATE tool_call_attempts SET status=?1, output_json=?2, settled_at=?3 \
         WHERE call_id=?4 AND status IN ('started','unknown')",
        params![
            if succeeded { "succeeded" } else { "failed" },
            encode_json(output)?,
            timestamp,
            durable_call_id,
        ],
    )?;
    let seq: i64 = tx.query_row(
        "SELECT COALESCE(MAX(seq), 0) + 1 FROM session_messages WHERE session_id=?1",
        params![session_id],
        |row| row.get(0),
    )?;
    let frame_seq: i64 = tx.query_row(
        "SELECT COALESCE(MAX(frame_seq), 0) + 1 FROM session_messages WHERE frame_id=?1",
        params![frame_id],
        |row| row.get(0),
    )?;
    let notice = format!(
        "[Reconciled external tool outcome]\nTool: {tool_name}\nCall ID: {call_id}\nStatus: {}\nObserved result: {}",
        if succeeded { "succeeded" } else { "failed" },
        encode_json(output)?,
    );
    let message = json!({"role": "system", "content": notice});
    tx.execute(
        "INSERT INTO session_messages (session_id, seq, run_id, frame_id, frame_seq, role, \
             content, harness_notice, created_at) VALUES (?1, ?2, ?3, ?4, ?5, 'system', ?6, 1, ?7)",
        params![
            session_id,
            seq,
            run_id,
            frame_id,
            frame_seq,
            encode_json(&message)?,
            timestamp
        ],
    )?;
    tx.execute(
        "UPDATE session_runs SET end_seq=?1 WHERE run_id=?2",
        params![seq, run_id],
    )?;
    append_event_in_transaction(
        &tx,
        &NewSessionEvent {
            session_id: session_id.clone(),
            run_id: Some(run_id.into()),
            frame_id: Some(frame_id.clone()),
            kind: SessionEventKind::ToolCallSettled,
            payload: json!({"call_id": call_id, "status": if succeeded {"succeeded"} else {"failed"}, "reconciled": true}),
        },
        &timestamp,
    )?;
    append_event_in_transaction(
        &tx,
        &NewSessionEvent {
            session_id: session_id.clone(),
            run_id: Some(run_id.into()),
            frame_id: Some(frame_id.clone()),
            kind: SessionEventKind::MessageAppended,
            payload: json!({"message_seq": seq, "frame_seq": frame_seq, "role": "system", "content": message, "harness_notice": true}),
        },
        &timestamp,
    )?;
    let unresolved: i64 = tx.query_row(
        "SELECT COUNT(*) FROM tool_call_attempts WHERE run_id=?1 AND status IN ('started','unknown') \
           AND effect_class='external_side_effect'",
        params![run_id],
        |row| row.get(0),
    )?;
    let ready = unresolved == 0;
    if ready {
        tx.execute(
            "UPDATE session_runs SET status='interrupted', kind='interrupted', \
                 error='External tool outcome reconciled; Run is ready to resume' WHERE run_id=?1",
            params![run_id],
        )?;
        tx.execute(
            "UPDATE sessions SET status='interrupted', updated_at=?1 WHERE id=?2",
            params![timestamp, session_id],
        )?;
        append_event_in_transaction(
            &tx,
            &NewSessionEvent {
                session_id,
                run_id: Some(run_id.into()),
                frame_id: Some(frame_id),
                kind: SessionEventKind::InputResolved,
                payload: json!({"kind": "tool_reconciliation", "call_id": call_id, "ready_to_resume": true}),
            },
            &timestamp,
        )?;
    }
    tx.commit()?;
    Ok(ready)
}

pub fn list_frame_messages(conn: &Connection, frame_id: &str) -> Result<Vec<Value>, DbError> {
    let mut stmt = conn
        .prepare("SELECT content FROM session_messages WHERE frame_id=?1 ORDER BY frame_seq, id")?;
    let rows = stmt
        .query_map(params![frame_id], |row| parse_json(row.get(0)?))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Atomically accepts work on any durable Frame without relying on the root Session's in-memory
/// message cursor. This is the concurrency-safe entry point used by child runtimes.
pub fn accept_frame_run(
    conn: &Connection,
    request: &AcceptFrameRun,
) -> Result<PersistAttemptLease, DbError> {
    let tx = conn.unchecked_transaction()?;
    let frame = tx
        .query_row(
            &format!("SELECT {FRAME_COLUMNS} FROM execution_frames WHERE id=?1"),
            params![request.frame_id],
            map_frame,
        )
        .optional()?
        .ok_or_else(|| DbError::NotFound(format!("frame {}", request.frame_id)))?;
    if frame.activity == "closed" || frame.active_run_id.is_some() {
        return Err(DbError::Conflict(format!(
            "frame {} is not idle",
            request.frame_id
        )));
    }
    let session_ordinal: i64 = tx.query_row(
        "SELECT COALESCE(MAX(ordinal), 0) + 1 FROM session_runs WHERE session_id=?1",
        params![frame.session_id],
        |row| row.get(0),
    )?;
    let frame_ordinal: i64 = tx.query_row(
        "SELECT COALESCE(MAX(frame_ordinal), 0) + 1 FROM session_runs WHERE actor_frame_id=?1",
        params![request.frame_id],
        |row| row.get(0),
    )?;
    let session_seq: i64 = tx.query_row(
        "SELECT COALESCE(MAX(seq), 0) FROM session_messages WHERE session_id=?1",
        params![frame.session_id],
        |row| row.get(0),
    )?;
    let frame_seq: i64 = tx.query_row(
        "SELECT COALESCE(MAX(frame_seq), 0) FROM session_messages WHERE frame_id=?1",
        params![request.frame_id],
        |row| row.get(0),
    )?;
    let start_seq = (!request.messages.is_empty()).then_some(session_seq + 1);
    let end_seq =
        (!request.messages.is_empty()).then_some(session_seq + request.messages.len() as i64);
    tx.execute(
        "INSERT INTO session_runs (run_id, session_id, ordinal, frame_id, actor_frame_id, \
             frame_ordinal, trigger_kind, task_summary, plan_mode, status, start_seq, end_seq, started_at) \
         VALUES (?1, ?2, ?3, ?4, ?4, ?5, ?6, ?7, 0, 'processing', ?8, ?9, ?10)",
        params![
            request.run_id,
            frame.session_id,
            session_ordinal,
            request.frame_id,
            frame_ordinal,
            request.trigger_kind,
            request.task_summary,
            start_seq,
            end_seq,
            request.started_at,
        ],
    )?;
    let attempt = PersistAttemptLease {
        attempt_id: Uuid::new_v4().to_string(),
        lease_token: Uuid::new_v4().to_string(),
        lease_owner: request.lease_owner.clone(),
        lease_expires_at: request.lease_expires_at.clone(),
    };
    tx.execute(
        "INSERT INTO run_attempts (attempt_id, run_id, attempt_no, lease_owner, lease_token, \
             lease_expires_at, status, started_at) VALUES (?1, ?2, 1, ?3, ?4, ?5, 'running', ?6)",
        params![
            attempt.attempt_id,
            request.run_id,
            attempt.lease_owner,
            attempt.lease_token,
            attempt.lease_expires_at,
            request.started_at,
        ],
    )?;
    tx.execute(
        "UPDATE session_runs SET active_attempt_id=?1 WHERE run_id=?2",
        params![attempt.attempt_id, request.run_id],
    )?;
    tx.execute(
        "UPDATE execution_frames SET activity='running', active_run_id=?1, revision=revision+1, \
             updated_at=?2 WHERE id=?3 AND active_run_id IS NULL",
        params![request.run_id, request.started_at, request.frame_id],
    )?;
    append_event_in_transaction(
        &tx,
        &NewSessionEvent {
            session_id: frame.session_id.clone(),
            run_id: Some(request.run_id.clone()),
            frame_id: Some(request.frame_id.clone()),
            kind: SessionEventKind::RunAccepted,
            payload: json!({"trigger_kind": request.trigger_kind, "task_summary": request.task_summary}),
        },
        &request.started_at,
    )?;
    append_event_in_transaction(
        &tx,
        &NewSessionEvent {
            session_id: frame.session_id.clone(),
            run_id: Some(request.run_id.clone()),
            frame_id: Some(request.frame_id.clone()),
            kind: SessionEventKind::AttemptStarted,
            payload: json!({"attempt_id": attempt.attempt_id, "attempt_no": 1, "lease_owner": attempt.lease_owner}),
        },
        &request.started_at,
    )?;
    for (offset, message) in request.messages.iter().enumerate() {
        let seq = session_seq + offset as i64 + 1;
        let local_seq = frame_seq + offset as i64 + 1;
        tx.execute(
            "INSERT INTO session_messages (session_id, seq, run_id, frame_id, frame_seq, role, \
                 content, harness_notice, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                frame.session_id,
                seq,
                request.run_id,
                request.frame_id,
                local_seq,
                message.role,
                encode_json(&message.content)?,
                if message.harness_notice { 1 } else { 0 },
                request.started_at,
            ],
        )?;
        append_event_in_transaction(
            &tx,
            &NewSessionEvent {
                session_id: frame.session_id.clone(),
                run_id: Some(request.run_id.clone()),
                frame_id: Some(request.frame_id.clone()),
                kind: SessionEventKind::MessageAppended,
                payload: json!({"message_seq": seq, "frame_seq": local_seq, "role": message.role, "content": message.content, "harness_notice": message.harness_notice}),
            },
            &request.started_at,
        )?;
    }
    tx.commit()?;
    Ok(attempt)
}

pub fn settle_frame_run(
    conn: &Connection,
    run_id: &str,
    frame_id: &str,
    attempt: &PersistAttemptLease,
    status: &str,
    response_message: Option<&FrameRunMessage>,
    result_payload: &Value,
) -> Result<(), DbError> {
    let tx = conn.unchecked_transaction()?;
    let frame = tx
        .query_row(
            &format!("SELECT {FRAME_COLUMNS} FROM execution_frames WHERE id=?1"),
            params![frame_id],
            map_frame,
        )
        .optional()?
        .ok_or_else(|| DbError::NotFound(format!("frame {frame_id}")))?;
    let owned: i64 = tx.query_row(
        "SELECT COUNT(*) FROM run_attempts a JOIN session_runs r ON r.run_id=a.run_id \
         WHERE a.attempt_id=?1 AND a.run_id=?2 AND a.lease_token=?3 AND a.status='running' \
           AND r.active_attempt_id=a.attempt_id AND r.actor_frame_id=?4",
        params![attempt.attempt_id, run_id, attempt.lease_token, frame_id],
        |row| row.get(0),
    )?;
    if owned != 1 {
        return Err(DbError::Conflict(
            "stale child attempt settle refused".into(),
        ));
    }
    let timestamp = now();
    let mut end_seq: Option<i64> = tx.query_row(
        "SELECT end_seq FROM session_runs WHERE run_id=?1",
        params![run_id],
        |row| row.get(0),
    )?;
    if let Some(message) = response_message {
        let seq: i64 = tx.query_row(
            "SELECT COALESCE(MAX(seq), 0) + 1 FROM session_messages WHERE session_id=?1",
            params![frame.session_id],
            |row| row.get(0),
        )?;
        let frame_seq: i64 = tx.query_row(
            "SELECT COALESCE(MAX(frame_seq), 0) + 1 FROM session_messages WHERE frame_id=?1",
            params![frame_id],
            |row| row.get(0),
        )?;
        tx.execute(
            "INSERT INTO session_messages (session_id, seq, run_id, frame_id, frame_seq, role, \
                 content, harness_notice, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                frame.session_id,
                seq,
                run_id,
                frame_id,
                frame_seq,
                message.role,
                encode_json(&message.content)?,
                if message.harness_notice { 1 } else { 0 },
                timestamp
            ],
        )?;
        append_event_in_transaction(
            &tx,
            &NewSessionEvent {
                session_id: frame.session_id.clone(),
                run_id: Some(run_id.into()),
                frame_id: Some(frame_id.into()),
                kind: SessionEventKind::MessageAppended,
                payload: json!({"message_seq": seq, "frame_seq": frame_seq, "role": message.role, "content": message.content, "harness_notice": message.harness_notice}),
            },
            &timestamp,
        )?;
        end_seq = Some(seq);
    }
    let attempt_status = match status {
        "completed" | "success" => "completed",
        "cancelled" => "cancelled",
        "interrupted" => "interrupted",
        "needs_reconciliation" => "needs_reconciliation",
        _ => "failed",
    };
    tx.execute(
        "UPDATE run_attempts SET status=?1, ended_at=?2 WHERE attempt_id=?3 AND lease_token=?4",
        params![
            attempt_status,
            timestamp,
            attempt.attempt_id,
            attempt.lease_token
        ],
    )?;
    tx.execute(
        "UPDATE session_runs SET status=?1, kind=?2, active_attempt_id=NULL, end_seq=?3, completed_at=?4 \
         WHERE run_id=?5",
        params![status, attempt_status, end_seq, timestamp, run_id],
    )?;
    tx.execute(
        "UPDATE execution_frames SET activity=?1, active_run_id=NULL, revision=revision+1, updated_at=?2 \
         WHERE id=?3 AND active_run_id=?4",
        params![if attempt_status == "interrupted" || attempt_status == "needs_reconciliation" {"suspended"} else {"idle"}, timestamp, frame_id, run_id],
    )?;
    append_event_in_transaction(
        &tx,
        &NewSessionEvent {
            session_id: frame.session_id.clone(),
            run_id: Some(run_id.into()),
            frame_id: Some(frame_id.into()),
            kind: SessionEventKind::RunCompleted,
            payload: json!({"status": status}),
        },
        &timestamp,
    )?;
    append_event_in_transaction(
        &tx,
        &NewSessionEvent {
            session_id: frame.session_id.clone(),
            run_id: Some(run_id.into()),
            frame_id: Some(frame_id.into()),
            kind: SessionEventKind::AttemptSettled,
            payload: json!({"attempt_id": attempt.attempt_id, "status": attempt_status}),
        },
        &timestamp,
    )?;
    if let Some(parent_id) = frame.parent_frame_id.as_ref() {
        let result_id = Uuid::new_v4().to_string();
        tx.execute(
            "INSERT INTO child_results (id, parent_frame_id, child_frame_id, run_id, status, payload, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![result_id, parent_id, frame_id, run_id, status, encode_json(result_payload)?, timestamp],
        )?;
        tx.execute(
            "INSERT INTO frame_mailbox (id, session_id, sender_frame_id, recipient_frame_id, \
                 message_kind, payload, correlation_id, status, created_at) \
             VALUES (?1, ?2, ?3, ?4, 'child_landed', ?5, ?6, 'unread', ?7)",
            params![
                Uuid::new_v4().to_string(),
                frame.session_id,
                frame_id,
                parent_id,
                encode_json(&json!({"child_frame_id": frame_id, "run_id": run_id}))?,
                result_id,
                timestamp
            ],
        )?;
        append_event_in_transaction(
            &tx,
            &NewSessionEvent {
                session_id: frame.session_id,
                run_id: Some(run_id.into()),
                frame_id: Some(frame_id.into()),
                kind: SessionEventKind::ChildLanded,
                payload: json!({"parent_frame_id": parent_id, "result_id": result_id, "status": status}),
            },
            &timestamp,
        )?;
    }
    tx.commit()?;
    Ok(())
}

pub fn cancel_active_frame(conn: &Connection, frame_id: &str) -> Result<bool, DbError> {
    let active: Option<(String, PersistAttemptLease)> = conn
        .query_row(
            "SELECT r.run_id, a.attempt_id, a.lease_token, a.lease_owner, a.lease_expires_at \
             FROM execution_frames f JOIN session_runs r ON r.run_id=f.active_run_id \
             JOIN run_attempts a ON a.attempt_id=r.active_attempt_id \
             WHERE f.id=?1 AND a.status='running'",
            params![frame_id],
            |row| {
                Ok((
                    row.get(0)?,
                    PersistAttemptLease {
                        attempt_id: row.get(1)?,
                        lease_token: row.get(2)?,
                        lease_owner: row.get(3)?,
                        lease_expires_at: row.get(4)?,
                    },
                ))
            },
        )
        .optional()?;
    let Some((run_id, attempt)) = active else {
        return Ok(false);
    };
    settle_frame_run(
        conn,
        &run_id,
        frame_id,
        &attempt,
        "cancelled",
        None,
        &json!({"cancelled": true, "reason": "stopped_by_parent"}),
    )?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;
    use serde_json::json;

    use super::*;
    use crate::repo::{
        append_history_checkpoint, create_session, persist_run, PersistAttemptLease,
        PersistCheckpointRequest, PersistMessage, PersistRunRequest,
    };

    fn database() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        crate::schema::apply_migrations(&mut conn).unwrap();
        create_session(&conn, "root", "/tmp/root", None, None).unwrap();
        conn
    }

    #[test]
    fn child_topology_is_durable_and_only_direct_edges_can_message() {
        let conn = database();
        let child = create_child_frame(
            &conn,
            &NewChildFrame {
                id: "child".into(),
                parent_frame_id: "root".into(),
                kind: "delegate".into(),
                profile_id: None,
                hidden: false,
                workspace_scope_id: Some("/tmp/root/child".into()),
            },
        )
        .unwrap();
        assert_eq!(child.root_frame_id, "root");
        assert_eq!(child.parent_frame_id.as_deref(), Some("root"));
        post_mailbox_message(&conn, "root", "child", "info", &json!({"text":"go"}), None).unwrap();
        assert_eq!(list_unread_mailbox(&conn, "child").unwrap().len(), 1);
    }

    #[test]
    fn child_landing_is_collect_only_and_exactly_once_per_parent() {
        let conn = database();
        create_child_frame(
            &conn,
            &NewChildFrame {
                id: "child".into(),
                parent_frame_id: "root".into(),
                kind: "delegate".into(),
                profile_id: None,
                hidden: false,
                workspace_scope_id: None,
            },
        )
        .unwrap();
        conn.execute(
            "INSERT INTO session_runs (run_id, session_id, ordinal, frame_id, actor_frame_id, \
                 frame_ordinal, task_summary, status, started_at) \
             VALUES ('child-run','root',1,'child','child',1,'task','completed','now')",
            [],
        )
        .unwrap();
        land_child_result(
            &conn,
            "child",
            "child-run",
            "completed",
            &json!({"answer":42}),
        )
        .unwrap();
        assert_eq!(
            list_unread_mailbox(&conn, "root").unwrap()[0].message_kind,
            "child_landed"
        );
        assert_eq!(
            collect_child_results(&conn, "root", "child").unwrap().len(),
            1
        );
        assert!(collect_child_results(&conn, "root", "child")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn stale_attempt_cannot_checkpoint_or_settle_after_ownership_is_fenced() {
        let conn = database();
        let lease = PersistAttemptLease {
            attempt_id: "attempt-1".into(),
            lease_token: "secret-1".into(),
            lease_owner: "worker-1".into(),
            lease_expires_at: "2099-01-01T00:00:00Z".into(),
        };
        append_history_checkpoint(
            &conn,
            &PersistCheckpointRequest {
                run_id: "run-1".into(),
                session_id: "root".into(),
                frame_id: "root".into(),
                task_summary: "task".into(),
                plan_mode: false,
                status: "processing".into(),
                awaiting: None,
                pending_ask_json: None,
                plan_data: None,
                compaction_state: None,
                title: None,
                started_at: "2026-08-20T00:00:00Z".into(),
                expected_count: 0,
                messages: vec![PersistMessage {
                    role: "user".into(),
                    content: r#"{"role":"user","content":"task"}"#.into(),
                    harness_notice: false,
                }],
                accepted_event_payload: Some(json!({"request":{"prompt":"task"}})),
                resumed_event_payload: None,
                attempt: Some(lease.clone()),
            },
        )
        .unwrap();
        let mut stale = lease.clone();
        stale.lease_token = "wrong".into();
        let error = persist_run(
            &conn,
            &PersistRunRequest {
                run_id: "run-1".into(),
                session_id: "root".into(),
                frame_id: "root".into(),
                task_summary: "task".into(),
                plan_mode: false,
                status: "completed".into(),
                kind: Some("natural".into()),
                awaiting: None,
                pending_ask_json: None,
                error: None,
                input_tokens: 1,
                output_tokens: 1,
                iterations: 1,
                plan_data: None,
                compaction_state: None,
                title: None,
                started_at: "2026-08-20T00:00:00Z".into(),
                checkpoint_start_seq: Some(1),
                messages: Vec::new(),
                attempt: Some(stale),
            },
        )
        .unwrap_err();
        assert!(matches!(error, DbError::Conflict(_)));
        persist_run(
            &conn,
            &PersistRunRequest {
                run_id: "run-1".into(),
                session_id: "root".into(),
                frame_id: "root".into(),
                task_summary: "task".into(),
                plan_mode: false,
                status: "completed".into(),
                kind: Some("natural".into()),
                awaiting: None,
                pending_ask_json: None,
                error: None,
                input_tokens: 1,
                output_tokens: 1,
                iterations: 1,
                plan_data: None,
                compaction_state: None,
                title: None,
                started_at: "2026-08-20T00:00:00Z".into(),
                checkpoint_start_seq: Some(1),
                messages: Vec::new(),
                attempt: Some(lease),
            },
        )
        .unwrap();
        let frame = get_frame(&conn, "root").unwrap().unwrap();
        assert_eq!(frame.activity, "idle");
        assert_eq!(frame.active_run_id, None);
    }

    #[test]
    fn parked_run_resumes_on_a_new_attempt_without_changing_frame_or_run_identity() {
        let conn = database();
        let first = PersistAttemptLease {
            attempt_id: "attempt-1".into(),
            lease_token: "token-1".into(),
            lease_owner: "worker".into(),
            lease_expires_at: "2099-01-01T00:00:00Z".into(),
        };
        append_history_checkpoint(
            &conn,
            &PersistCheckpointRequest {
                run_id: "run-parked".into(),
                session_id: "root".into(),
                frame_id: "root".into(),
                task_summary: "plan".into(),
                plan_mode: true,
                status: "processing".into(),
                awaiting: None,
                pending_ask_json: None,
                plan_data: None,
                compaction_state: None,
                title: None,
                started_at: "2026-08-20T00:00:00Z".into(),
                expected_count: 0,
                messages: vec![PersistMessage {
                    role: "user".into(),
                    content: r#"{"role":"user","content":"plan"}"#.into(),
                    harness_notice: false,
                }],
                accepted_event_payload: Some(json!({"request":{"prompt":"plan"}})),
                resumed_event_payload: None,
                attempt: Some(first.clone()),
            },
        )
        .unwrap();
        persist_run(
            &conn,
            &PersistRunRequest {
                run_id: "run-parked".into(),
                session_id: "root".into(),
                frame_id: "root".into(),
                task_summary: "plan".into(),
                plan_mode: true,
                status: "awaiting_user_response".into(),
                kind: Some("awaiting".into()),
                awaiting: Some("user_response".into()),
                pending_ask_json: Some("{}".into()),
                error: None,
                input_tokens: 1,
                output_tokens: 1,
                iterations: 1,
                plan_data: None,
                compaction_state: None,
                title: None,
                started_at: "2026-08-20T00:00:00Z".into(),
                checkpoint_start_seq: Some(1),
                messages: Vec::new(),
                attempt: Some(first),
            },
        )
        .unwrap();
        let second = PersistAttemptLease {
            attempt_id: "attempt-2".into(),
            lease_token: "token-2".into(),
            lease_owner: "worker".into(),
            lease_expires_at: "2099-01-01T00:00:00Z".into(),
        };
        append_history_checkpoint(
            &conn,
            &PersistCheckpointRequest {
                run_id: "run-parked".into(),
                session_id: "root".into(),
                frame_id: "root".into(),
                task_summary: "answer".into(),
                plan_mode: false,
                status: "processing".into(),
                awaiting: None,
                pending_ask_json: None,
                plan_data: None,
                compaction_state: None,
                title: None,
                started_at: "2026-08-20T00:01:00Z".into(),
                expected_count: 1,
                messages: vec![PersistMessage {
                    role: "user".into(),
                    content: r#"{"role":"user","content":"answer"}"#.into(),
                    harness_notice: false,
                }],
                accepted_event_payload: None,
                resumed_event_payload: Some(json!({"request":{"prompt":"answer"}})),
                attempt: Some(second),
            },
        )
        .unwrap();
        let run_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM session_runs WHERE run_id='run-parked'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let attempt_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM run_attempts WHERE run_id='run-parked'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(run_count, 1);
        assert_eq!(attempt_count, 2);
        let frame = get_frame(&conn, "root").unwrap().unwrap();
        assert_eq!(frame.active_run_id.as_deref(), Some("run-parked"));
        assert_eq!(frame.activity, "running");
    }

    #[test]
    fn external_side_effect_requires_explicit_result_before_same_run_can_resume() {
        let conn = database();
        let request = AcceptFrameRun {
            run_id: "run-reconcile".into(),
            frame_id: "root".into(),
            task_summary: "publish".into(),
            trigger_kind: "user".into(),
            started_at: "2026-08-20T00:00:00Z".into(),
            lease_owner: "worker".into(),
            lease_expires_at: "2099-01-01T00:00:00Z".into(),
            messages: vec![FrameRunMessage {
                role: "user".into(),
                content: json!({"role":"user","content":"publish"}),
                harness_notice: false,
            }],
        };
        let attempt = accept_frame_run(&conn, &request).unwrap();
        record_tool_call_started(
            &conn,
            &ToolCallStart {
                call_id: "call-1",
                run_id: "run-reconcile",
                attempt_id: &attempt.attempt_id,
                lease_token: &attempt.lease_token,
                tool_name: "publish",
                effect_class: "external_side_effect",
                input: &json!({"document":"paper"}),
                idempotency_key: None,
            },
        )
        .unwrap();
        record_tool_call_uncertain(
            &conn,
            "call-1",
            "run-reconcile",
            &attempt.attempt_id,
            &attempt.lease_token,
            "response timeout",
            &json!({"outcome":"unknown"}),
        )
        .unwrap();
        let unresolved = list_unresolved_tool_calls(&conn, "run-reconcile").unwrap();
        assert_eq!(unresolved.len(), 1);
        assert_eq!(unresolved[0].call_id, "call-1");
        assert_eq!(unresolved[0].status, "unknown");
        persist_run(
            &conn,
            &PersistRunRequest {
                run_id: "run-reconcile".into(),
                session_id: "root".into(),
                frame_id: "root".into(),
                task_summary: "publish".into(),
                plan_mode: false,
                status: "needs_reconciliation".into(),
                kind: Some("reconciliation".into()),
                awaiting: Some("tool_reconciliation".into()),
                pending_ask_json: None,
                error: Some("external result unknown".into()),
                input_tokens: 1,
                output_tokens: 1,
                iterations: 1,
                plan_data: None,
                compaction_state: None,
                title: None,
                started_at: "2026-08-20T00:00:00Z".into(),
                checkpoint_start_seq: Some(1),
                messages: Vec::new(),
                attempt: Some(attempt),
            },
        )
        .unwrap();

        assert!(resolve_tool_reconciliation(
            &conn,
            "run-reconcile",
            "call-1",
            true,
            &json!({"publication_id":"p-1"}),
        )
        .unwrap());
        let status: String = conn
            .query_row(
                "SELECT status FROM session_runs WHERE run_id='run-reconcile'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(status, "interrupted");
        let messages = list_frame_messages(&conn, "root").unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[1]["role"], "system");
        assert!(messages[1]["content"]
            .as_str()
            .unwrap()
            .contains("publication_id"));

        let resumed = start_attempt(&conn, "run-reconcile", "worker-2", 60).unwrap();
        assert_eq!(resumed.attempt_no, 2);
        let frame = get_frame(&conn, "root").unwrap().unwrap();
        assert_eq!(frame.active_run_id.as_deref(), Some("run-reconcile"));
    }
}
