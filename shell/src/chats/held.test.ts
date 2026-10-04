import { beforeEach, describe, expect, it } from "vitest";
import { heldMessage, holdMessage, resetHeld, takeHeld } from "./held";

beforeEach(() => resetHeld());

describe("held messages", () => {
  it("holds nothing until something is said", () => {
    expect(heldMessage("c-1")).toBeNull();
  });

  it("appends a second thing said to the first, rather than queueing two turns", () => {
    holdMessage("c-1", { text: "fix the parser", images: [] });
    holdMessage("c-1", { text: "and run the tests", images: [] });

    expect(heldMessage("c-1")?.text).toBe("fix the parser\n\nand run the tests");
  });

  it("keeps each conversation's words apart", () => {
    holdMessage("c-1", { text: "one", images: [] });
    holdMessage("c-2", { text: "two", images: [] });

    expect(heldMessage("c-1")?.text).toBe("one");
    expect(heldMessage("c-2")?.text).toBe("two");
  });

  it("caps the pictures at the limit it is given", () => {
    const picture = { media_type: "image/png", data: "AAAA" };
    holdMessage("c-1", { text: "", images: [picture, picture] }, 3);
    holdMessage("c-1", { text: "look", images: [picture, picture] }, 3);

    expect(heldMessage("c-1")?.images).toHaveLength(3);
    expect(heldMessage("c-1")?.text).toBe("look");
  });

  it("hands the words out exactly once", () => {
    holdMessage("c-1", { text: "once", images: [] });

    expect(takeHeld("c-1")?.text).toBe("once");
    expect(takeHeld("c-1")).toBeNull();
    expect(heldMessage("c-1")).toBeNull();
  });
});
