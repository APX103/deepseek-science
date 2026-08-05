import { describe, expect, test } from "bun:test";
import {
  appendStreamText,
  appendStreamThinking,
  appendStreamToolCall,
  appendStreamToolResult,
  getStreamSnapshot,
  resetStreamDraft,
  startStream,
} from "../src/store";

describe("stream draft reset", () => {
  test("review retries discard text/thinking but retain committed tool evidence", () => {
    const sid = "reviewer-draft-reset";
    startStream(sid);
    appendStreamThinking(sid, "private first reasoning");
    appendStreamText(sid, "rejected first draft");
    appendStreamToolCall(sid, [
      { id: "call-1", name: "read_file", input: { path: "paper.md" } },
    ]);
    appendStreamToolResult(sid, [
      { tool_use_id: "call-1", content: "evidence", is_error: false },
    ]);

    resetStreamDraft(sid);
    const reset = getStreamSnapshot(sid);
    expect(reset?.text).toBe("");
    expect(reset?.thinking).toBe("");
    expect(reset?.toolCalls).toHaveLength(1);
    expect(reset?.toolCalls[0]?.content).toBe("evidence");

    appendStreamText(sid, "final revised answer");
    expect(getStreamSnapshot(sid)?.text).toBe("final revised answer");
  });
});
