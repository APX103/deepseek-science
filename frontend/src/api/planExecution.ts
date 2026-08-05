import type {
  AwaitingKind,
  Plan,
  RunKind,
  SessionRun,
} from "../types";

/**
 * The small part of StreamBuffer needed to determine the current terminal
 * wait reason. Keeping this structural avoids coupling the pure helpers to the
 * frontend store.
 */
export interface LiveRunState {
  running: boolean;
  kind: RunKind | null;
  awaiting: AwaitingKind;
}

export interface ComposerSendIntent {
  planMode: boolean;
  executePlan: boolean;
}

function latestRun(runs: readonly SessionRun[]): SessionRun | undefined {
  return runs.reduce<SessionRun | undefined>(
    (latest, run) => (!latest || run.ordinal > latest.ordinal ? run : latest),
    undefined,
  );
}

/**
 * Resolve the exact current wait reason from run-level state. A live stream
 * owns the state while it exists: an in-flight or naturally completed stream
 * must not inherit an older persisted wait reason. Without a live stream, the
 * highest-ordinal persisted run is authoritative.
 *
 * Deliberately do not accept SessionStatus here. The coarse `awaiting` status
 * cannot distinguish plan approval from an ask_user response.
 */
export function currentAwaitingKind(
  live: LiveRunState | undefined,
  runs: readonly SessionRun[] = [],
): AwaitingKind {
  if (live) {
    if (live.running || live.kind !== "awaiting") return null;
    return live.awaiting;
  }

  const persisted = latestRun(runs);
  return persisted?.kind === "awaiting" ? persisted.awaiting : null;
}

/**
 * An approved plan remains executable until every step is done. Failed steps
 * are deliberately retryable; a transport/provider failure must not strand a
 * durably approved plan after reload.
 */
export function canExecuteApprovedPlan(plan: Plan | null | undefined): boolean {
  return !!plan?.approved && plan.steps.some((step) => step.status !== "done");
}

/** An ask_user pause is resumed by answering, never by the generic plan CTA. */
export function canExecutePlanNow(
  plan: Plan | null | undefined,
  awaiting: AwaitingKind,
  running: boolean,
): boolean {
  return !running && awaiting !== "user_response" && canExecuteApprovedPlan(plan);
}

/**
 * Route a composer submission without losing a plan that was paused by
 * ask_user. Approved incomplete plans resume execution with the answer.
 * Legacy/conflicting unapproved plans re-enter planning so the answer can be
 * incorporated into a replacement plan instead of silently clearing it.
 */
export function composerSendIntent(
  awaiting: AwaitingKind,
  plan: Plan | null | undefined,
  requestedPlanMode: boolean,
): ComposerSendIntent {
  if (awaiting === "user_response") {
    if (canExecuteApprovedPlan(plan)) {
      return { planMode: false, executePlan: true };
    }
    if (plan && !plan.approved) {
      return { planMode: true, executePlan: false };
    }
  }

  return { planMode: requestedPlanMode, executePlan: false };
}

/**
 * Human-readable plan status must be derived from step completion, not merely
 * from approval. Approval starts execution; it does not mean the work is done.
 */
export function planStatusLabel(
  plan: Plan,
  awaitingApproval: boolean,
  running: boolean,
): string {
  if (!plan.approved) return awaitingApproval ? "等待批准" : "生成中";

  const complete =
    plan.steps.length > 0 && plan.steps.every((step) => step.status === "done");
  if (complete) return "已完成";
  return running ? "执行中" : "已批准 · 等待执行";
}
