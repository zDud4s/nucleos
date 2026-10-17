import { describe, expect, it } from "vitest";

import { clearDraft, readDraft, readScroll, saveDraft, saveScroll } from "./drafts";

describe("drafts", () => {
  it("drafts are kept per conversation and dropped when emptied", () => {
    expect(readDraft("d-a")).toEqual({ text: "", images: [] });

    saveDraft("d-a", { text: "first words", images: [] });
    saveDraft("d-b", { text: "other words", images: [] });
    expect(readDraft("d-a").text).toBe("first words");
    expect(readDraft("d-b").text).toBe("other words");

    // Emptied text and no pictures deletes the entry: the read is the empty draft again.
    saveDraft("d-a", { text: "", images: [] });
    expect(readDraft("d-a")).toEqual({ text: "", images: [] });
    expect(readDraft("d-b").text).toBe("other words");

    clearDraft("d-b");
    expect(readDraft("d-b")).toEqual({ text: "", images: [] });
  });

  it("scroll positions are kept per conversation", () => {
    expect(readScroll("s-a")).toBeNull();
    saveScroll("s-a", 120);
    saveScroll("s-b", 0);
    expect(readScroll("s-a")).toBe(120);
    expect(readScroll("s-b")).toBe(0);
    expect(readScroll("s-c")).toBeNull();
  });
});
