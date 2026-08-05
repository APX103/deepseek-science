import { describe, expect, test } from "bun:test";
import { buildRunPayload } from "../src/api/sessionRun";

describe("session run intent", () => {
  test("ordinary and fresh-plan requests cannot inherit an approved plan", () => {
    expect(buildRunPayload("follow up", { runId: "run-follow-up" })).toEqual({
      run_id: "run-follow-up",
      prompt: "follow up",
      plan_mode: false,
      execute_plan: false,
    });
    expect(buildRunPayload("make a new plan", { planMode: true, runId: "run-plan" })).toEqual({
      run_id: "run-plan",
      prompt: "make a new plan",
      plan_mode: true,
      execute_plan: false,
    });
  });

  test("post-approval execution is explicit", () => {
    expect(buildRunPayload("execute", { executePlan: true, runId: "run-execute" })).toEqual({
      run_id: "run-execute",
      prompt: "execute",
      plan_mode: false,
      execute_plan: true,
    });
  });
});
