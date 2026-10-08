// @vitest-environment node
import { describe, expect, it } from "vitest";
import { commandAt, mentionAt, orchestrateTask, withCommand, withMention } from "./mention";

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
  // eventually, and a file list over `helena@gmail` is the feature getting in the way.
  it("is nothing when the @ is inside a word", () => {
    expect(mentionAt("manda para helena@gmail", 23)).toBeNull();
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

/* ------------------------------------------------------------- commandAt -- */

describe("commandAt", () => {
  it("finds a command being typed at the start of the box", () => {
    expect(commandAt("/comm", 5)).toEqual({ query: "comm", from: 0, to: 5 });
  });

  it("finds a bare slash as an empty query, which is where the list opens", () => {
    expect(commandAt("/", 1)).toEqual({ query: "", from: 0, to: 1 });
  });

  // The CLI only expands a slash command at the very start of a message. A picker that opened
  // mid-sentence would offer to insert something that then does nothing at all.
  it("is nothing when the slash is not the first character", () => {
    expect(commandAt("olha /comm", 10)).toBeNull();
    expect(commandAt(" /comm", 6)).toBeNull();
  });

  it("is nothing once the name is finished and an argument has begun", () => {
    expect(commandAt("/commit a mensagem", 18)).toBeNull();
  });

  // A namespace is part of the name, and typing the colon must not close the list.
  it("keeps reading through a namespace separator", () => {
    expect(commandAt("/superpowers:brain", 18)).toEqual({
      query: "superpowers:brain",
      from: 0,
      to: 18,
    });
  });

  it("stops at the caret", () => {
    expect(commandAt("/commit", 4)).toEqual({ query: "com", from: 0, to: 4 });
  });
});

describe("withCommand", () => {
  it("writes the command and a space, so an argument can follow", () => {
    const next = withCommand("/comm", { query: "comm", from: 0, to: 5 }, "commit");

    expect(next.text).toBe("/commit ");
    expect(next.caret).toBe(8);
  });

  it("keeps whatever was after the caret", () => {
    const next = withCommand("/comm tudo", { query: "comm", from: 0, to: 5 }, "commit");

    expect(next.text).toBe("/commit  tudo");
    expect(next.caret).toBe(8);
  });
});

/* ------------------------------------------------------- orchestrateTask -- */

describe("orchestrateTask", () => {
  it("reads the task out of /orchestrate <task> and nothing else", () => {
    expect(orchestrateTask("/orchestrate fix the login")).toBe("fix the login");
    expect(orchestrateTask("  /orchestrate   fix it\nand the logout  ")).toBe("fix it\nand the logout");
    // No task is not a request, and a longer command name is another command.
    expect(orchestrateTask("/orchestrate")).toBeNull();
    expect(orchestrateTask("/orchestrate   ")).toBeNull();
    expect(orchestrateTask("/orchestrated fix")).toBeNull();
    expect(orchestrateTask("please /orchestrate fix")).toBeNull();
    expect(orchestrateTask("fix the login")).toBeNull();
  });
});
