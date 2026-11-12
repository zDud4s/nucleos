// @vitest-environment node
import { describe, expect, it } from "vitest";

import { revise, type Provisional } from "./provisional";

describe("the microphone's own region of a text box", () => {
  it("opens the region at the end of what was already typed", () => {
    const { text, region, caret } = revise("hello", null, "world");
    expect(text).toBe("hello world");
    expect(region).toEqual<Provisional>({ at: 5, length: 6, lead: " " });
    expect(caret).toBe(11);
  });

  it("adds no space to an empty box, nor to one already ending in one", () => {
    expect(revise("", null, "one").text).toBe("one");
    expect(revise("typed ", null, "one").text).toBe("typed one");
    expect(revise("typed\n", null, "one").text).toBe("typed\none");
  });

  it("replaces the whole region rather than appending to it", () => {
    const first = revise("note:", null, "come");
    const second = revise(first.text, first.region, "come view");
    expect(second.text).toBe("note: come view");
    expect(second.region).toEqual<Provisional>({ at: 5, length: 10, lead: " " });
    expect(second.caret).toBe(15);
  });

  /* The revision that shortens is the one a naive splice gets wrong: "câmbio" arriving after
     "câmbio e" has to leave the box shorter than it found it, not overwrite the first six
     characters and strand " e" on the end. */
  it("shrinks the box when the model changes its mind downwards", () => {
    const first = revise("", null, "câmbio e");
    const second = revise(first.text, first.region, "câmbio");
    expect(second.text).toBe("câmbio");
  });

  it("takes its own space back with its last word", () => {
    const first = revise("typed", null, "misheard");
    const second = revise(first.text, first.region, "");
    expect(second.text).toBe("typed");
    expect(second.region).toBeNull();
    expect(second.caret).toBe(5);
  });

  /* Silence before a first word has nothing to replace, so it must not open a region — an empty
     one would fix a separator in the box that the next revision would then have to live with. */
  it("opens nothing for a revision that heard nothing", () => {
    const nothing = revise("typed", null, "");
    expect(nothing.text).toBe("typed");
    expect(nothing.region).toBeNull();
  });

  /* The region is a span, not a suffix. Somebody who clicks in front of the microphone's words and
     types keeps their text; today the caller closes the region on any keystroke, so this is the
     guard on that staying true rather than a behaviour anyone reaches. */
  it("leaves what follows the region alone", () => {
    const revised = revise("a bc", { at: 1, length: 1, lead: " " }, "and");
    expect(revised.text).toBe("a andbc");
  });
});
