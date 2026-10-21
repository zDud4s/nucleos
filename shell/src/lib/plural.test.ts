// @vitest-environment node
import { describe, expect, it } from "vitest";
import { plural, pluralWord } from "./plural";

describe("plural", () => {
  it("counts the noun, singular only for exactly one", () => {
    expect(plural(1, "file")).toBe("1 file");
    expect(plural(3, "file")).toBe("3 files");
    expect(plural(0, "file")).toBe("0 files");
  });

  it("takes the irregular plural when the -s is wrong", () => {
    expect(plural(2, "person", "people")).toBe("2 people");
    expect(plural(1, "person", "people")).toBe("1 person");
  });
});

describe("pluralWord", () => {
  it("is the noun alone", () => {
    expect(pluralWord(1, "file")).toBe("file");
    expect(pluralWord(2, "file")).toBe("files");
    expect(pluralWord(5, "person", "people")).toBe("people");
  });
});
