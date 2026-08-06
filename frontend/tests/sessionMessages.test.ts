import { describe, expect, test } from "bun:test";
import {
  attachSessionRuns,
  restoreVisibleMessages,
  type PersistedSessionMessage,
} from "../src/api/sessionMessages";
import { HIDDEN_ASSISTANT_PROTOCOL_NOTICE } from "../src/api/assistantProtocol";
import type { SessionRun } from "../src/types";
import dsmlDisplayCorpus from "../../test-fixtures/dsml-display-corpus.json";

const RAW_PYTHON_DSML = `<｜｜DSML｜｜tool_calls>
<｜｜DSML｜invoke name="python">
<｜DSML｜parameter name="code" string="true">
# 3. agenda checks
print("private protocol payload")
</｜DSML｜parameter>
</｜｜DSML｜invoke>
</｜｜DSML｜tool_calls>`;

describe("restoreVisibleMessages", () => {
  test("filters system, harness, internal, unknown, and role-mismatched rows", () => {
    const rows: PersistedSessionMessage[] = [
      {
        role: "system",
        content: { role: "system", content: "system secret" },
        harness_notice: false,
      },
      {
        role: "assistant",
        content: { role: "assistant", content: "harness secret" },
        harness_notice: true,
      },
      {
        role: "assistant",
        content: { role: "system", content: "mismatched inner secret" },
        harness_notice: false,
      },
      {
        role: "assistant",
        content: { role: "assistant", content: "internal secret", internal: true },
        harness_notice: false,
      },
      {
        role: "assistant",
        content: {
          role: "assistant",
          content: "metadata internal secret",
          metadata: { internal: true },
        },
        harness_notice: false,
      },
      {
        role: "developer",
        content: { role: "developer", content: "developer secret" },
        harness_notice: false,
      },
      {
        role: "assistant",
        content: {
          role: "assistant",
          reasoning_content: "visible reasoning",
          content: "visible answer",
          usage: { input_tokens: 7, output_tokens: 3 },
          metadata: { trace_id: "must-not-leak" },
        },
        harness_notice: false,
      },
    ];

    const restored = restoreVisibleMessages(rows);
    expect(restored).toEqual([
      {
        role: "assistant",
        content: [
          { type: "thinking", thinking: "visible reasoning" },
          { type: "text", text: "visible answer" },
        ],
        usage: { input_tokens: 7, output_tokens: 3, cache_hit_tokens: 0, cache_miss_tokens: 0 },
      },
    ]);
    const serialized = JSON.stringify(restored);
    expect(serialized).not.toContain("secret");
    expect(serialized).not.toContain("trace_id");
    expect(serialized).not.toContain("harness_notice");
  });

  test("preserves whitespace and pairs normal assistant/tool history in two passes", () => {
    const rows: PersistedSessionMessage[] = [
      {
        role: "user",
        content: { role: "user", content: "  keep\nspacing  " },
        harness_notice: false,
      },
      {
        role: "assistant",
        content: {
          role: "assistant",
          reasoning_content: "checking",
          tool_calls: [
            {
              id: "call-1",
              function: { name: "read_file", arguments: '{"path":"paper.md"}' },
            },
          ],
        },
        harness_notice: false,
      },
      {
        role: "tool",
        content: {
          role: "tool",
          tool_call_id: "call-1",
          content: "paper contents",
          name: "read_file",
          is_error: true,
        },
        harness_notice: false,
      },
      {
        role: "assistant",
        content: { role: "assistant", content: "done" },
        harness_notice: false,
      },
    ];

    expect(restoreVisibleMessages(rows)).toEqual([
      { role: "user", content: "  keep\nspacing  " },
      {
        role: "assistant",
        content: [
          { type: "thinking", thinking: "checking" },
          {
            type: "tool_use",
            id: "call-1",
            name: "read_file",
            input: { path: "paper.md" },
          },
          {
            type: "tool_result",
            tool_use_id: "call-1",
            content: "paper contents",
            is_error: true,
          },
        ],
        usage: null,
      },
      { role: "assistant", content: "done", usage: null },
    ]);
  });

  test("sanitizes direct content blocks and ignores harness tool results", () => {
    const rows: PersistedSessionMessage[] = [
      {
        role: "assistant",
        content: [
          { type: "text", text: "visible", internal_metadata: "drop me" },
          { type: "private_state", value: "drop me too" },
          { type: "tool_use", id: "call-2", name: "bash", input: { command: "pwd" } },
        ],
        harness_notice: false,
      },
      {
        role: "tool",
        content: {
          role: "tool",
          tool_call_id: "call-2",
          content: "hidden harness result",
        },
        harness_notice: true,
      },
    ];

    const restored = restoreVisibleMessages(rows);
    expect(restored).toEqual([
      {
        role: "assistant",
        content: [
          { type: "text", text: "visible" },
          { type: "tool_use", id: "call-2", name: "bash", input: { command: "pwd" } },
        ],
        usage: null,
      },
    ]);
    expect(JSON.stringify(restored)).not.toContain("drop me");
    expect(JSON.stringify(restored)).not.toContain("hidden harness result");
  });

  test("hides complete historical DSML payloads while retaining trusted prose", () => {
    const rows: PersistedSessionMessage[] = [
      {
        role: "assistant",
        content: {
          role: "assistant",
          content: `# 可信前言\n\n${RAW_PYTHON_DSML}\n\n可信结论。`,
        },
      },
      {
        role: "assistant",
        content: [{ type: "text", text: RAW_PYTHON_DSML }],
      },
    ];

    const restored = restoreVisibleMessages(rows);
    const serialized = JSON.stringify(restored);
    expect(serialized).toContain("可信前言");
    expect(serialized).toContain("可信结论");
    expect(serialized).toContain(HIDDEN_ASSISTANT_PROTOCOL_NOTICE.slice(2));
    expect(serialized).not.toContain("DSML");
    expect(serialized).not.toContain("agenda checks");
    expect(serialized).not.toContain("private protocol payload");
    expect(serialized).not.toContain('"type":"tool_use"');
  });

  test("applies the same protocol shield to restored thinking fields", () => {
    const rows: PersistedSessionMessage[] = [
      {
        role: "assistant",
        content: {
          role: "assistant",
          reasoning_content: RAW_PYTHON_DSML,
          content: "safe answer",
        },
      },
      {
        role: "assistant",
        content: [{ type: "thinking", thinking: RAW_PYTHON_DSML }],
      },
    ];

    const serialized = JSON.stringify(restoreVisibleMessages(rows));
    expect(serialized).toContain("safe answer");
    expect(serialized).toContain(HIDDEN_ASSISTANT_PROTOCOL_NOTICE.slice(2));
    expect(serialized).not.toContain("DSML");
    expect(serialized).not.toContain("private protocol payload");
  });

  test("shields CommonMark ambiguity when restoring persisted assistant text", () => {
    for (const [source, secret] of [
      [dsmlDisplayCorpus.regressions.paragraph_indented_protocol, "INDENT_SECRET"],
      [dsmlDisplayCorpus.regressions.escaped_backticks_protocol, "ESCAPED_SECRET"],
    ] as const) {
      const restored = restoreVisibleMessages([
        { role: "assistant", content: { role: "assistant", content: source } },
      ]);
      const serialized = JSON.stringify(restored);
      expect(serialized).toContain(HIDDEN_ASSISTANT_PROTOCOL_NOTICE.slice(2));
      expect(serialized).not.toContain("DSML");
      expect(serialized).not.toContain(secret);
    }
  });

  test("fails closed for orphan and malformed historical protocol fragments", () => {
    const rows: PersistedSessionMessage[] = [
      {
        role: "assistant",
        content: {
          role: "assistant",
          content: "apparently safe\n</｜｜DSML｜｜parameter>\nsecret suffix",
        },
      },
      {
        role: "assistant",
        content: {
          role: "assistant",
          content:
            "trusted prefix\n<｜｜DSML｜｜tool_calls>\n<｜｜DSML｜｜invoke name=\"python\">\nsecret suffix",
        },
      },
    ];

    const restored = restoreVisibleMessages(rows);
    expect(restored[0]?.content).toBe(HIDDEN_ASSISTANT_PROTOCOL_NOTICE);
    expect(String(restored[1]?.content)).toContain("trusted prefix");
    const serialized = JSON.stringify(restored);
    expect(serialized).not.toContain("apparently safe");
    expect(serialized).not.toContain("DSML");
    expect(serialized).not.toContain("secret suffix");
  });

  test("preserves fenced and inline DSML documentation during restore", () => {
    const documentation = [
      "Inline `<||DSML||tool_calls>` remains documentation.",
      "",
      "```text",
      "<｜DSML｜tool_calls>",
      "<｜DSML｜invoke name=\"python\">",
      "```",
    ].join("\n");

    expect(
      restoreVisibleMessages([
        { role: "assistant", content: { role: "assistant", content: documentation } },
      ]),
    ).toEqual([{ role: "assistant", content: documentation, usage: null }]);
  });

  test("restores only the revised answer after an internal reviewer veto", () => {
    const rows: PersistedSessionMessage[] = [
      {
        role: "user",
        content: { role: "user", content: "research request" },
        harness_notice: false,
      },
      {
        role: "assistant",
        content: { role: "assistant", content: "rejected first draft" },
        harness_notice: true,
      },
      {
        role: "system",
        content: { role: "system", content: "reviewer correction notice" },
        harness_notice: true,
      },
      {
        role: "assistant",
        content: { role: "assistant", content: "final revised answer" },
        harness_notice: false,
      },
    ];

    expect(restoreVisibleMessages(rows)).toEqual([
      { role: "user", content: "research request" },
      { role: "assistant", content: "final revised answer", usage: null },
    ]);
  });

  test("scopes repeated provider tool ids by run and attaches durable run footers", () => {
    const rows: PersistedSessionMessage[] = [
      { seq: 1, run_id: "run-a", role: "user", content: { role: "user", content: "first" } },
      {
        seq: 2,
        run_id: "run-a",
        role: "assistant",
        content: {
          role: "assistant",
          tool_calls: [{ id: "call-1", function: { name: "read_file", arguments: '{"path":"a"}' } }],
        },
      },
      { seq: 3, run_id: "run-a", role: "tool", content: { role: "tool", tool_call_id: "call-1", content: "A" } },
      { seq: 4, run_id: "run-b", role: "user", content: { role: "user", content: "second" } },
      {
        seq: 5,
        run_id: "run-b",
        role: "assistant",
        content: {
          role: "assistant",
          tool_calls: [{ id: "call-1", function: { name: "read_file", arguments: '{"path":"b"}' } }],
        },
      },
      { seq: 6, run_id: "run-b", role: "tool", content: { role: "tool", tool_call_id: "call-1", content: "B" } },
    ];
    const baseRun = {
      ordinal: 1,
      frame_id: "frame",
      task_summary: "task",
      plan_mode: false,
      status: "completed",
      kind: "natural",
      awaiting: null,
      pending_ask: null,
      error: null,
      usage: { input_tokens: 1, output_tokens: 1 },
      iterations: 1,
      plan: null,
      started_at: "2026-08-04T00:00:00Z",
      completed_at: "2026-08-04T00:00:01Z",
    } satisfies Omit<SessionRun, "run_id" | "start_seq" | "end_seq">;
    const runs: SessionRun[] = [
      { ...baseRun, run_id: "run-a", start_seq: 1, end_seq: 3 },
      { ...baseRun, ordinal: 2, run_id: "run-b", start_seq: 4, end_seq: 6 },
    ];

    const restored = attachSessionRuns(restoreVisibleMessages(rows), runs);
    expect(JSON.stringify(restored[1]?.content)).toContain('"content":"A"');
    expect(JSON.stringify(restored[3]?.content)).toContain('"content":"B"');
    expect(restored[1]?.run?.run_id).toBe("run-a");
    expect(restored[3]?.run?.run_id).toBe("run-b");
    expect(restored[1]?.source_seq).toBe(3);
    expect(restored[3]?.source_seq).toBe(6);
  });
});
