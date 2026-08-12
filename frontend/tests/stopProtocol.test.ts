import { describe, expect, test } from "bun:test";
import {
  appendStreamText,
  beginStreamStop,
  completeStream,
  failStream,
  failStreamStop,
  finishStreamStop,
  getMessagesSnapshot,
  getStreamSnapshot,
  retireStreamAfterBackendFinish,
  resumeStreamAfterLateStop,
  sendUserMessage,
  setStreamAborter,
  startStream,
} from "../src/store";

describe("explicit Stop protocol", () => {
  test("Stop is scoped to the exact client run and unlocks only after ack", () => {
    const sid = "stop-ack";
    const runId = startStream(sid);

    expect(beginStreamStop(sid)).toBe(runId);
    expect(beginStreamStop(sid)).toBeNull();
    expect(getStreamSnapshot(sid)?.running).toBe(true);
    expect(getStreamSnapshot(sid)?.stopping).toBe(true);

    expect(finishStreamStop(sid, runId)).toBe(true);
    expect(getStreamSnapshot(sid)?.running).toBe(false);
    expect(getStreamSnapshot(sid)?.stopped).toBe(true);
  });

  test("transport failure during a terminal/Stop race cannot leave UI running", () => {
    const sid = "stop-transport-race";
    const runId = startStream(sid);
    expect(beginStreamStop(sid)).toBe(runId);

    failStream(sid, "SSE disconnected", runId);
    expect(getStreamSnapshot(sid)?.running).toBe(true);
    expect(getStreamSnapshot(sid)?.stopping).toBe(true);

    // Backend reports cancelled=false: natural terminal won, but its transport
    // has already failed. The deferred error must settle and unlock the UI.
    resumeStreamAfterLateStop(sid, runId);
    expect(getStreamSnapshot(sid)?.running).toBe(false);
    expect(getStreamSnapshot(sid)?.stopping).toBe(false);
    expect(getStreamSnapshot(sid)?.error).toBe("SSE disconnected");
  });

  test("normal terminal delivery wins over a concurrent late Stop response", () => {
    const sid = "stop-terminal-wins";
    const runId = startStream(sid);
    expect(beginStreamStop(sid)).toBe(runId);
    completeStream(sid, null, 1, "natural", null, null, null, null, undefined, runId);

    expect(resumeStreamAfterLateStop(sid, runId)).toBe(false);
    expect(getStreamSnapshot(sid)?.running).toBe(false);
    expect(getStreamSnapshot(sid)?.stopped).toBe(false);
    expect(getStreamSnapshot(sid)?.kind).toBe("natural");
  });

  test("a failed cancel request does not forget a simultaneous transport loss", () => {
    const sid = "stop-retry-after-transport-loss";
    const runId = startStream(sid);
    expect(beginStreamStop(sid)).toBe(runId);
    failStream(sid, "SSE lost during Stop", runId);
    failStreamStop(sid, "cancel endpoint unavailable", runId);

    expect(getStreamSnapshot(sid)?.running).toBe(true);
    expect(beginStreamStop(sid)).toBe(runId);
    resumeStreamAfterLateStop(sid, runId);
    expect(getStreamSnapshot(sid)?.running).toBe(false);
    expect(getStreamSnapshot(sid)?.error).toBe("SSE lost during Stop");
  });

  test("a late Stop acknowledgement cannot abort or unlock a newer run", () => {
    const sid = "stop-old-ack-new-run";
    const oldRunId = startStream(sid);
    expect(beginStreamStop(sid)).toBe(oldRunId);
    expect(
      completeStream(sid, null, 1, "natural", null, null, null, null, undefined, oldRunId),
    ).toBe(true);

    const newRunId = startStream(sid);
    let newAbortCount = 0;
    expect(setStreamAborter(sid, () => { newAbortCount += 1; }, newRunId)).toBe(true);

    expect(finishStreamStop(sid, oldRunId)).toBe(false);
    expect(failStreamStop(sid, "late failure", oldRunId)).toBe(false);
    expect(resumeStreamAfterLateStop(sid, oldRunId)).toBe(false);
    expect(newAbortCount).toBe(0);
    expect(getStreamSnapshot(sid)).toMatchObject({
      runId: newRunId,
      running: true,
      stopping: false,
      error: null,
    });
  });

  test("authoritative restore retirement drops the exact draft shell without duplicating it", () => {
    const sid = "stop-authoritative-retire";
    sendUserMessage(sid, "canonical user turn");
    const runId = startStream(sid);
    appendStreamText(sid, "partial draft that canonical GET already replaced", runId);
    let abortCount = 0;
    setStreamAborter(sid, () => { abortCount += 1; }, runId);
    const restoredMessages = getMessagesSnapshot(sid);

    expect(retireStreamAfterBackendFinish(sid, runId)).toBe(true);
    expect(abortCount).toBe(1);
    expect(getStreamSnapshot(sid)).toBeUndefined();
    expect(getMessagesSnapshot(sid)).toEqual(restoredMessages);
    expect(JSON.stringify(getMessagesSnapshot(sid))).not.toContain("partial draft");
  });
});
