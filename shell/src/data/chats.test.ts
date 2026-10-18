import { describe, expect, it } from "vitest";
import { conversationInterval } from "./chats";
import { POLL } from "./poll";

describe("conversationInterval", () => {
  it("polls at the turn cadence while a turn is live", () => {
    expect(conversationInterval(true)).toBe(POLL.turn);
  });

  it("slows to the queue cadence once the conversation has settled", () => {
    expect(conversationInterval(false)).toBe(POLL.queue);
    expect(conversationInterval(false)).toBeGreaterThan(POLL.fast);
  });
});
