# ADR-001 — Durable Agent Frames

Status: accepted

## Decision

`Frame` is a durable agent conversation/actor, not a user turn and not a worker job.

- A `Session` owns one long-lived root Frame. The root Frame id is the Session id.
- A normal user follow-up creates a new `Run` on the same root Frame.
- An approval or answer continues the existing Run.
- A crash/restart creates a new `RunAttempt` for the same Run.
- An explicit retry creates a new Run with `retry_of_run_id`.
- Only explicit delegation, review, bookmarking, or branching creates a child Frame.
- A completed child remains addressable and resumable; a follow-up creates a new Run on it.
- Frame parentage is immutable and durable. In-memory child registries are caches only.

The resulting identity hierarchy is:

```text
Session -> Frame -> Run -> RunAttempt
                    ^
Job ----------------|
```

`AgentProfile` is configuration, `Frame` is identity, `Run` is logical work, `RunAttempt`
is leased execution, and `Job` is scheduling.

## State ownership

Frame activity is `idle | running | waiting | suspended | closed`. A Run owns its outcome:
`accepted | running | waiting | completed | failed | cancelled | interrupted |
needs_reconciliation`. Completion does not close a Frame.

At most one Run may be active on a Frame. At most one non-terminal Attempt may own a Run.
All Attempt writes are fenced by the durable attempt id and lease token. A stale worker may not
commit messages, tool results, or terminal state after ownership has moved.

## Recovery and side effects

Recovery starts from the last durable checkpoint. Read-only and explicitly idempotent tool calls
may be replayed. An external side effect with unknown outcome is never blindly replayed: its Run
enters `needs_reconciliation` until the effect is queried or a user chooses a resolution.

Child completion uses a durable result plus a small mailbox wake signal. Notifications are not the
result source of truth.

## Legacy migration

Historical `session_runs.frame_id` values were generated per turn by the old `Frame::begin_run`.
They must not be reinterpreted as an agent tree. Migration creates one root Frame per Session,
adds `actor_frame_id` to Runs, and records old identifiers in `legacy_frame_aliases`. Existing
events remain immutable; migration records an honest baseline instead of inventing old events.

## Consequences

The old mutable-frame-id behavior is removed. Frame-local transcripts, child execution, durable
mailboxes, lease reconciliation, and audit projections can now share one stable identity model.
