export interface StreamRunOptions {
  planMode?: boolean;
  executePlan?: boolean;
  runId?: string;
}

export function createRunId(): string {
  return crypto.randomUUID();
}

/** Build the explicit run intent sent to the backend.
 * Ordinary prompts never inherit a previously approved or cancelled plan. */
export function buildRunPayload(
  prompt: string,
  options: StreamRunOptions = {},
) {
  return {
    run_id: options.runId ?? createRunId(),
    prompt,
    plan_mode: options.planMode ?? false,
    execute_plan: options.executePlan ?? false,
  };
}
