import { describe, expect, test } from "bun:test";
import { DSS_API_TOKEN_HEADER, withApiToken } from "../src/api/auth";

describe("API capability headers", () => {
  test("adds the in-memory token while preserving request headers", () => {
    const headers = withApiToken("launch-secret", {
      "Content-Type": "application/json",
    });

    expect(headers.get(DSS_API_TOKEN_HEADER)).toBe("launch-secret");
    expect(headers.get("Content-Type")).toBe("application/json");
  });

  test("does not invent a token for direct browser or CLI development", () => {
    const headers = withApiToken(undefined, { Accept: "application/json" });

    expect(headers.has(DSS_API_TOKEN_HEADER)).toBe(false);
    expect(headers.get("Accept")).toBe("application/json");
  });
});
