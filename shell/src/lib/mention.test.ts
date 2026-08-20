import { describe, expect, it } from "vitest";
import { mentionAt, withMention } from "./mention";

/* ------------------------------------------------------------- mentionAt -- */

describe("mentionAt", () => {
  it("finds what has been typed after an @ at the caret", () => {
    const at = mentionAt("olha o @pars", 12);

    expect(at).toEqual({ query: "pars", from: 7, to: 12 });
  });

  it("finds a bare @ as an empty query, because that is where the list opens", () => {
    // Not "no mention": pressing @ is the gesture, and answering it with nothing on screen is
    // the one moment the feature has to prove it exists.
    expect(mentionAt("olha o @", 8)).toEqual({ query: "", from: 7, to: 8 });
  });

  // A caret sitting after a finished word is not a mention being typed. Treating it as one would
  // pop a list over somebody who has moved on, on a keystroke they did not aim at it.
  it("is nothing when there is no @ before the caret", () => {
    expect(mentionAt("olha o parser", 13)).toBeNull();
  });

  it("is nothing once a space has been typed past the @", () => {
    expect(mentionAt("olha o @pars er", 15)).toBeNull();
    expect(mentionAt("olha o @ er", 11)).toBeNull();
  });

  // An email address is the reason this cannot simply look for the last @: everybody types one
  // eventually, and a file list over `duarte@gmail` is the feature getting in the way.
  it("is nothing when the @ is inside a word", () => {
    expect(mentionAt("manda para duarte@gmail", 23)).toBeNull();
  });

  it("reads the @ nearest the caret, not the first one in the box", () => {
    const at = mentionAt("@core/src e agora @runn", 23);

    expect(at).toEqual({ query: "runn", from: 18, to: 23 });
  });

  // The caret is not always at the end. Text after it belongs to the message, not to the query.
  it("stops at the caret and ignores what comes after it", () => {
    expect(mentionAt("@pars e o resto", 5)).toEqual({ query: "pars", from: 0, to: 5 });
  });

  it("finds a mention at the very start of the box", () => {
    expect(mentionAt("@", 1)).toEqual({ query: "", from: 0, to: 1 });
  });

  // A path is a normal thing to be part way through typing, and a slash must not end the mention.
  it("keeps reading through a path separator", () => {
    expect(mentionAt("@core/src/par", 13)).toEqual({ query: "core/src/par", from: 0, to: 13 });
  });
});

/* ------------------------------------------------------------ withMention -- */

describe("withMention", () => {
  it("puts the chosen path where the half-typed name was, and a space after it", () => {
    // The space is not cosmetic: without it the caret sits against the path and the next thing
    // typed becomes part of it, which is how a picked file turns back into a typo.
    const next = withMention("olha o @pars", { query: "pars", from: 7, to: 12 }, "core/src/parser.rs");

    expect(next.text).toBe("olha o @core/src/parser.rs ");
    expect(next.caret).toBe(27);
  });

  it("keeps whatever was after the caret", () => {
    const next = withMention("@pars e o resto", { query: "pars", from: 0, to: 5 }, "core/src/parser.rs");

    expect(next.text).toBe("@core/src/parser.rs  e o resto");
    expect(next.caret).toBe(20);
  });

  // A directory is a place to keep typing, not a thing to have finished naming.
  it("leaves the caret inside a folder rather than closing it off", () => {
    const next = withMention("@cor", { query: "cor", from: 0, to: 4 }, "core/src", true);

    expect(next.text).toBe("@core/src/");
    expect(next.caret).toBe(10);
  });
});
