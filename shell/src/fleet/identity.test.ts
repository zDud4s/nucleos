// @vitest-environment node
import { describe, expect, it } from "vitest";
import { identityOf } from "./identity";
import { child, node } from "./test-fixtures";

describe("identityOf", () => {
  it("says harness · model, and marks a child whose harness differs from its parent's", () => {
    const parent = node("p", { harness: "claude-code", model: "opus-5.5" });
    const same = child("a", parent, { harness: "claude-code", model: "haiku-5.5" });
    const other = child("b", parent, { harness: "codex-cli", model: "gpt-5-codex" });
    expect(identityOf(parent, undefined)).toEqual({ text: "claude-code · opus-5.5", crossHarness: false });
    expect(identityOf(same, parent.harness).crossHarness).toBe(false);
    expect(identityOf(other, parent.harness)).toEqual({ text: "codex-cli · gpt-5-codex", crossHarness: true });
  });
  it("drops the model when there is none", () => {
    expect(identityOf(node("p", { model: null }), undefined).text).toBe("claude-code");
  });
});
