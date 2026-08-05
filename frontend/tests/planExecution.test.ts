import { describe, expect, test } from "bun:test";
import {
  canExecuteApprovedPlan,
  canExecutePlanNow,
  composerSendIntent,
  currentAwaitingKind,
  planStatusLabel,
} from "../src/api/planExecution";
import type { Plan, SessionRun } from "../src/types";

function plan(approved: boolean, statuses: Plan["steps"][number]["status"][]): Plan {
  return {
    approved,
    steps: statuses.map((status, index) => ({ title: `step ${index}`, status })),
  };
}

function run(overrides: Partial<SessionRun> = {}): SessionRun {
  return {
    run_id: "run-1",
    ordinal: 1,
    frame_id: "frame-1",
    task_summary: "test",
    plan_mode: false,
    status: "awaiting",
    kind: "awaiting",
    awaiting: "user_response",
    pending_ask: null,
    error: null,
    usage: { input_tokens: 0, output_tokens: 0 },
    iterations: 1,
    plan: null,
    start_seq: 1,
    end_seq: 2,
    started_at: "2026-08-05T00:00:00Z",
    completed_at: "2026-08-05T00:00:01Z",
    ...overrides,
  };
}

describe("exact awaiting state", () => {
  test("a completed live stream overrides a stale persisted wait reason", () => {
    expect(
      currentAwaitingKind(
        { running: false, kind: "awaiting", awaiting: "user_response" },
        [run({ awaiting: "plan_approval" })],
      ),
    ).toBe("user_response");

    expect(
      currentAwaitingKind(
        { running: false, kind: "natural", awaiting: null },
        [run({ awaiting: "plan_approval" })],
      ),
    ).toBeNull();
  });

  test("an active live stream never inherits an older persisted wait reason", () => {
    expect(
      currentAwaitingKind(
        { running: true, kind: null, awaiting: null },
        [run({ awaiting: "plan_approval" })],
      ),
    ).toBeNull();
  });

  test("restored state uses the highest-ordinal persisted run, not session status", () => {
    expect(
      currentAwaitingKind(undefined, [
        run({ run_id: "new", ordinal: 3, awaiting: "user_response" }),
        run({ run_id: "old", ordinal: 1, awaiting: "plan_approval" }),
      ]),
    ).toBe("user_response");
    expect(
      currentAwaitingKind(undefined, [
        run({ ordinal: 1, awaiting: "plan_approval" }),
        run({ ordinal: 2, kind: "natural", status: "awaiting", awaiting: null }),
      ]),
    ).toBeNull();
  });
});

describe("approved plan execution retry", () => {
  test("approval is executable after reload while work remains", () => {
    expect(canExecuteApprovedPlan(plan(true, ["pending", "done"]))).toBe(true);
    expect(canExecuteApprovedPlan(plan(true, ["running"]))).toBe(true);
  });

  test("failed work remains retryable but a completed plan does not", () => {
    expect(canExecuteApprovedPlan(plan(true, ["failed"]))).toBe(true);
    expect(canExecuteApprovedPlan(plan(true, ["done", "done"]))).toBe(false);
    expect(canExecuteApprovedPlan(plan(false, ["pending"]))).toBe(false);
    expect(canExecuteApprovedPlan(null)).toBe(false);
  });

  test("approval is never presented as completion before every step is done", () => {
    expect(planStatusLabel(plan(false, ["pending"]), true, false)).toBe("等待批准");
    expect(planStatusLabel(plan(true, ["pending", "pending"]), false, true)).toBe("执行中");
    expect(planStatusLabel(plan(true, ["done", "pending"]), false, false)).toBe(
      "已批准 · 等待执行",
    );
    expect(planStatusLabel(plan(true, ["done", "done"]), false, false)).toBe("已完成");
  });

  test("the generic execute control is hidden while ask_user awaits an answer", () => {
    const approved = plan(true, ["done", "pending"]);
    expect(canExecutePlanNow(approved, "user_response", false)).toBe(false);
    expect(canExecutePlanNow(approved, "plan_execution", false)).toBe(true);
    expect(canExecutePlanNow(approved, null, true)).toBe(false);
  });
});

describe("composer answer routing", () => {
  test("a live ask_user answer resumes an approved incomplete plan", () => {
    const awaiting = currentAwaitingKind({
      running: false,
      kind: "awaiting",
      awaiting: "user_response",
    });
    expect(
      composerSendIntent(awaiting, plan(true, ["done", "pending"]), true),
    ).toEqual({ planMode: false, executePlan: true });
  });

  test("a restored legacy user_response with an unapproved plan has no plan CTA and replans", () => {
    const legacyPlan = plan(false, ["pending"]);
    const awaiting = currentAwaitingKind(undefined, [
      run({ awaiting: "user_response", plan: legacyPlan }),
    ]);
    expect(awaiting).not.toBe("plan_approval");
    expect(canExecutePlanNow(legacyPlan, awaiting, false)).toBe(false);
    expect(composerSendIntent(awaiting, legacyPlan, false)).toEqual({
      planMode: true,
      executePlan: false,
    });
  });

  test("an answer without a plan follows the ordinary composer mode", () => {
    expect(composerSendIntent("user_response", null, false)).toEqual({
      planMode: false,
      executePlan: false,
    });
    expect(composerSendIntent(null, null, true)).toEqual({
      planMode: true,
      executePlan: false,
    });
  });
});
