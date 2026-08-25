import { describe, expect, it } from "vitest";
import { blocks, lines, spans } from "./rich";

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

/* ------------------------------------------------------------------ tables -- */

describe("a table, which used to be five rows of pipes", () => {
  it("reads a head, a body and the alignment the author asked for", () => {
    const read = blocks(
      [
        "| ano  | divisivel por | bissexto |",
        "| :--- | ------------- | -------: |",
        "| 1900 | 4, 100        | nao      |",
        "| 2000 | 4, 100, 400   | sim      |",
      ].join("\n"),
    );

    expect(read).toEqual([
      {
        kind: "table",
        head: ["ano", "divisivel por", "bissexto"],
        align: ["left", null, "right"],
        rows: [
          ["1900", "4, 100", "nao"],
          ["2000", "4, 100, 400", "sim"],
        ],
      },
    ]);
  });

  it("is not a table without the row that says so", () => {
    // A shell pipeline, an or-pattern, an ASCII sketch. Pipes in a sentence are pipes in a
    // sentence, and a renderer that drew a table out of one would be inventing structure.
    expect(blocks("| grep foo | wc -l |")).toEqual([
      { kind: "prose", text: "| grep foo | wc -l |" },
    ]);
  });

  it("keeps an escaped pipe inside its cell", () => {
    const read = blocks(["| comando |", "| ------- |", "| a \\| b   |"].join("\n"));

    expect(read).toEqual([
      { kind: "table", head: ["comando"], align: [null], rows: [["a | b"]] },
    ]);
  });

  it("does not find a table inside a fenced block", () => {
    // The one place a pipe-and-dash drawing is most likely to appear, and the one place it must
    // stay exactly as it was written.
    const drawn = "```\n| a | b |\n| - | - |\n```";

    expect(blocks(drawn)).toEqual([
      { kind: "code", text: "| a | b |\n| - | - |", lang: null },
    ]);
  });

  it("keeps the prose on either side of it", () => {
    const read = blocks("antes\n| a |\n| - |\n| 1 |\ndepois");

    expect(read.map((block) => block.kind)).toEqual(["prose", "table", "prose"]);
  });
});

/* -------------------------------------------------------------- what a line is -- */

describe("what a line of prose is", () => {
  it("keeps the blank line somebody typed", () => {
    // The paragraph break. The parser always emitted it and the renderer drew it at zero height,
    // so every gap in every answer went missing and three sections came out as one block.
    expect(lines("um\n\ndois").map((line) => line.kind)).toEqual([
      "line",
      "blank",
      "line",
    ]);
  });

  it("counts a heading's level, up to six", () => {
    expect(lines("# um\n### tres\n###### seis").map((line) => line.kind === "heading" && line.level))
      .toEqual([1, 3, 6]);
  });

  it("keeps an author's own numbering rather than correcting it", () => {
    // A model that writes 1, 2, 2, 3 wrote that. Renumbering would be editing the answer.
    const read = lines("1. um\n2. dois\n2. dois outra vez");

    expect(read.map((line) => (line.kind === "bullet" ? line.marker : null))).toEqual([
      "1.",
      "2.",
      "2.",
    ]);
  });

  it("takes a dash and a star as the same unmarked bullet", () => {
    const read = lines("- um\n* dois\n+ tres");

    expect(read.every((line) => line.kind === "bullet" && line.marker === null)).toBe(true);
  });

  it("nests by the indents the list actually uses, not by a fixed number of spaces", () => {
    // Two spaces per level and four are both ordinary. A rule that fixed on one would read a
    // four-space author's first level as a second.
    const twos = lines("- um\n  - dentro\n    - mais dentro\n- dois");
    const fours = lines("- um\n    - dentro\n        - mais dentro\n- dois");
    const depths = (read: ReturnType<typeof lines>) =>
      read.map((line) => (line.kind === "bullet" ? line.depth : null));

    expect(depths(twos)).toEqual([0, 1, 2, 0]);
    expect(depths(fours)).toEqual([0, 1, 2, 0]);
  });

  it("does not flatten a nested item because there was a blank line above it", () => {
    // A list with air between its items is still one list.
    const read = lines("- um\n\n  - dentro");

    expect(read.map((line) => (line.kind === "bullet" ? line.depth : line.kind))).toEqual([
      0,
      "blank",
      1,
    ]);
  });

  it("ends a list at the paragraph after it", () => {
    const read = lines("  - dentro\numa frase\n- outra vez");

    expect(read.map((line) => (line.kind === "bullet" ? line.depth : line.kind))).toEqual([
      0,
      "line",
      0,
    ]);
  });

  it("reads a quote and a rule as themselves", () => {
    expect(lines("> citado\n\n---").map((line) => line.kind)).toEqual([
      "quote",
      "blank",
      "rule",
    ]);
  });

  it("does not read a dashed bullet as a rule", () => {
    // `- x` is an item and `---` is a rule, and the difference is the space and the dashes.
    expect(lines("- um").map((line) => line.kind)).toEqual(["bullet"]);
  });
});

/* ------------------------------------------------------------- links and emphasis -- */

describe("links, and the schemes that never become one", () => {
  it("reads a link as its words and its address", () => {
    expect(spans("ver [o calendario](https://exemplo.pt/x) aqui")).toEqual([
      { kind: "plain", text: "ver " },
      { kind: "link", text: "o calendario", href: "https://exemplo.pt/x" },
      { kind: "plain", text: " aqui" },
    ]);
  });

  it("takes mailto as well, because the opener does", () => {
    expect(spans("[escreve](mailto:a@b.pt)")).toEqual([
      { kind: "link", text: "escreve", href: "mailto:a@b.pt" },
    ]);
  });

  it("refuses every other scheme, and leaves the characters where they were", () => {
    // The one that matters: a transcript is text from a model, and a `javascript:` URL must never
    // become something this window can be made to open. It stays visible instead.
    for (const hostile of [
      "[carrega](javascript:alert(1))",
      "[carrega](data:text/html,<script>)",
      "[carrega](file:///C:/Windows)",
      "[carrega](vscode://extension/evil)",
    ]) {
      expect(spans(hostile)).toEqual([{ kind: "plain", text: hostile }]);
    }
  });

  it("leaves a bracket that opens nothing as the character it is", () => {
    expect(spans("um [ sozinho")).toEqual([{ kind: "plain", text: "um [ sozinho" }]);
    expect(spans("[sem endereco]()")).toEqual([{ kind: "plain", text: "[sem endereco]()" }]);
  });
});

describe("emphasis, and the names it must not eat", () => {
  it("reads a single star as italic", () => {
    expect(spans("sao *tres* regras")).toEqual([
      { kind: "plain", text: "sao " },
      { kind: "em", text: "tres" },
      { kind: "plain", text: " regras" },
    ]);
  });

  it("never turns a snake_case name into italics", () => {
    // The reason `flanks` exists. This app is full of names like these, and an underscore between
    // two word characters is part of a name, not a delimiter.
    expect(spans("budget_usd e turn_budget_usd")).toEqual([
      { kind: "plain", text: "budget_usd e turn_budget_usd" },
    ]);
    expect(spans("de year_start ate year_end")).toEqual([
      { kind: "plain", text: "de year_start ate year_end" },
    ]);
  });

  it("does not read arithmetic as emphasis", () => {
    // A star with a space on the inside cannot open or close.
    expect(spans("2 * 3 * 4")).toEqual([{ kind: "plain", text: "2 * 3 * 4" }]);
  });

  it("keeps bold as bold rather than reading it as two italics", () => {
    expect(spans("**mesmo** importante")).toEqual([
      { kind: "strong", text: "mesmo" },
      { kind: "plain", text: " importante" },
    ]);
  });

  it("leaves an unclosed opener as the character it is", () => {
    expect(spans("um * sozinho")).toEqual([{ kind: "plain", text: "um * sozinho" }]);
    expect(spans("**sem fecho")).toEqual([{ kind: "plain", text: "**sem fecho" }]);
  });

  it("takes inline code before anything else in it", () => {
    // A name inside backticks is a name, whatever characters it contains.
    expect(spans("`budget_usd * 2`")).toEqual([{ kind: "code", text: "budget_usd * 2" }]);
  });
});
