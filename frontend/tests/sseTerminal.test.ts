import { afterEach, describe, expect, test } from "bun:test";
import { connectSSE } from "../src/api/client";

const originalFetch = globalThis.fetch;
const originalWindow = (globalThis as { window?: unknown }).window;
const originalLocalStorage = (globalThis as { localStorage?: unknown }).localStorage;

function installBrowserGlobals() {
  (globalThis as { window?: unknown }).window = {};
  (globalThis as { localStorage?: unknown }).localStorage = {
    getItem: () => null,
    setItem: () => undefined,
  };
}

afterEach(() => {
  globalThis.fetch = originalFetch;
  if (originalWindow === undefined) delete (globalThis as { window?: unknown }).window;
  else (globalThis as { window?: unknown }).window = originalWindow;
  if (originalLocalStorage === undefined) {
    delete (globalThis as { localStorage?: unknown }).localStorage;
  } else {
    (globalThis as { localStorage?: unknown }).localStorage = originalLocalStorage;
  }
});

describe("SSE terminal boundary", () => {
  test("dispatches the first terminal only and cancels unread late frames", async () => {
    installBrowserGlobals();
    const encoded = new TextEncoder().encode(
      [
        'data: {"type":"text","text":"before"}',
        'data: {"type":"complete","kind":"natural","iterations":1}',
        'data: {"type":"text","text":"late"}',
        'data: {"type":"error","message":"late error"}',
        "",
      ].join("\n\n"),
    );
    let cancelled = false;
    const body = new ReadableStream<Uint8Array>({
      start(controller) {
        controller.enqueue(encoded);
      },
      cancel() {
        cancelled = true;
      },
    });
    globalThis.fetch = async () => new Response(body, { status: 200 });

    const received: string[] = [];
    let resolveComplete: (() => void) | undefined;
    const complete = new Promise<void>((resolve) => { resolveComplete = resolve; });
    connectSSE("terminal-test", "prompt", {
      onText: (value) => received.push(`text:${value}`),
      onComplete: () => {
        received.push("complete");
        resolveComplete?.();
      },
      onError: (message) => received.push(`error:${message}`),
    });

    await Promise.race([
      complete,
      new Promise<never>((_, reject) => setTimeout(() => reject(new Error("SSE timeout")), 250)),
    ]);
    await new Promise((resolve) => setTimeout(resolve, 0));

    expect(received).toEqual(["text:before", "complete"]);
    expect(cancelled).toBe(true);
  });

  test("does not convert a throwing terminal handler into a second error", async () => {
    installBrowserGlobals();
    let cancelled = false;
    const body = new ReadableStream<Uint8Array>({
      start(controller) {
        controller.enqueue(
          new TextEncoder().encode(
            'data: {"type":"complete","kind":"natural","iterations":1}\n\n',
          ),
        );
      },
      cancel() {
        cancelled = true;
      },
    });
    globalThis.fetch = async () => new Response(body, { status: 200 });

    const errors: string[] = [];
    connectSSE("throwing-terminal-test", "prompt", {
      onComplete: () => { throw new Error("consumer failed"); },
      onError: (message) => errors.push(message),
    });
    await new Promise((resolve) => setTimeout(resolve, 10));

    expect(cancelled).toBe(true);
    expect(errors).toEqual([]);
  });
});
