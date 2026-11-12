// @vitest-environment node
import { describe, expect, it } from "vitest";
import type { ReactFlowState } from "@xyflow/react";
import { selectCard } from "./zoom";

const at = (zoom: number) => ({ transform: [0, 0, zoom] }) as unknown as ReactFlowState;

describe("selectCard", () => {
  it("turns a node into a card strictly below 60% zoom", () => {
    expect(selectCard(at(0.59))).toBe(true);
    expect(selectCard(at(0.6))).toBe(false);
    expect(selectCard(at(1))).toBe(false);
  });
});
