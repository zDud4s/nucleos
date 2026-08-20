import { describe, expect, it } from "vitest";
import { blocks, spans } from "./rich";

/**
 * What a model writes is markdown, and until now this app drew it as characters.
 *
 * Not a markdown library: a small, closed set of shapes that a chat answer actually uses, parsed
 * into data. Nothing here produces markup — the component decides what an element is, so no
 * transcript can ever talk this renderer into emitting HTML it was handed.
 */
describe("the shapes an answer is written in", () => {
  it("keeps a fenced block whole, and apart from the prose around it", () => {
    const read = blocks("antes\n```sh\ncargo test\n```\ndepois");

    expect(read).toEqual([
      { kind: "prose", text: "antes" },
      { kind: "code", text: "cargo test", lang: "sh" },
      { kind: "prose", text: "depois" },
    ]);
  });

  it("reads a fence nobody closed as code to the end, rather than losing it", () => {
    // The live case: a fenced block half-written, polled mid-stream. Treating the opener as prose
    // would redraw the whole tail as paragraphs and then snap it back to code when the fence lands.
    const read = blocks("olha\n```\nnao fechei");

    expect(read).toEqual([
      { kind: "prose", text: "olha" },
      { kind: "code", text: "nao fechei", lang: null },
    ]);
  });

  it("says a block has no language rather than inventing one", () => {
    expect(blocks("```\nx\n```")).toEqual([{ kind: "code", text: "x", lang: null }]);
  });

  it("is one piece of prose when nothing is fenced", () => {
    expect(blocks("so texto")).toEqual([{ kind: "prose", text: "so texto" }]);
  });

  it("drops nothing when the text is empty", () => {
    expect(blocks("")).toEqual([]);
  });
});

describe("what a line of prose is made of", () => {
  it("pulls inline code out of the words around it", () => {
    expect(spans("corre `cargo test` agora")).toEqual([
      { kind: "plain", text: "corre " },
      { kind: "code", text: "cargo test" },
      { kind: "plain", text: " agora" },
    ]);
  });

  it("pulls bold out too", () => {
    expect(spans("isto e **importante**")).toEqual([
      { kind: "plain", text: "isto e " },
      { kind: "strong", text: "importante" },
    ]);
  });

  it("leaves a lone backtick as the character it is", () => {
    // A price, a shell snippet somebody half-typed, a stray tick. Swallowing the rest of the line
    // into code because one tick opened is worse than showing the tick.
    expect(spans("um ` sozinho")).toEqual([{ kind: "plain", text: "um ` sozinho" }]);
  });

  it("is nothing at all for an empty line", () => {
    expect(spans("")).toEqual([]);
  });
});
