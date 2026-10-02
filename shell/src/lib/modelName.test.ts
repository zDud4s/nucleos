import { describe, expect, it } from "vitest";
import { displayName } from "./modelName";

describe("displayName", () => {
  it("formats vendor ids as product names", () => {
    const table: [string, string][] = [
      ["claude-sonnet-5-5", "Sonnet 5.5"],
      ["claude-sonnet-5.5", "Sonnet 5.5"],
      ["claude-opus-4-1-20250805", "Opus 4.1"],
      ["claude-3-5-sonnet-20241022", "Sonnet 3.5"],
      ["claude-haiku-4-5", "Haiku 4.5"],
      ["sonnet", "Sonnet"],
      ["gpt-5.6-sol", "GPT-5.6 Sol"],
      ["gpt-5.5", "GPT-5.5"],
      ["gpt-4o-mini", "GPT-4o Mini"],
      ["o4-mini", "o4 Mini"],
      ["  Claude-Opus-4-6-latest ", "Opus 4.6"],
    ];
    for (const [id, want] of table) expect(displayName(id)).toBe(want);
  });
});
