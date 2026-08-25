/**
 * What a model writes, parsed into the shapes it actually uses.
 *
 * Not a markdown library, and deliberately not: this app draws text that came out of somebody
 * else's transcript, and the rule everywhere it does that is *text, never markup*. So nothing here
 * emits HTML or accepts it — the parser returns data, the component decides what an element is, and
 * a transcript containing `<script>` is a string containing `<script>` at every step.
 *
 * The set is closed, and it grew once, on evidence. It was fenced code, inline code, bold,
 * headings and bullets, on the argument that a chat answer leans on those five and that each shape
 * added is another way to mangle text that was fine as it was. Then a real answer was photographed
 * in the app: a numbered list came out as plain sentences, a nested bullet came out at its parent's
 * level, a table came out as five rows of pipes, and a link came out as its own square brackets
 * followed by a URL that wrapped mid-word. The argument was right about the risk and wrong about
 * the set — those are not exotic shapes, they are what an agent writes when it explains anything.
 *
 * So the set is now: fenced code, tables, headings, bullets (ordered and not, nested), quotes,
 * rules, and inline code, bold, italic and links. Everything outside it stays the characters it
 * was, and every opener without a closer stays the character it is.
 *
 * Kept beside `turns.ts` for the same reason that one is: shape, not fetching, and testable without
 * mounting anything.
 */

/** A run of prose, one fenced code block, or one table. */
export type Block =
  | { kind: "prose"; text: string }
  | { kind: "code"; text: string; lang: string | null }
  | { kind: "table"; head: string[]; rows: string[][]; align: Align[] };

/** How a table column is set. `null` is the author saying nothing about it. */
export type Align = "left" | "center" | "right" | null;

/** A piece of one line of prose. */
export type Span =
  | { kind: "plain"; text: string }
  | { kind: "code"; text: string }
  | { kind: "strong"; text: string }
  | { kind: "em"; text: string }
  /**
   * A link, whose `href` is already known to be openable.
   *
   * The scheme is checked in `linkAt` and nowhere else: `[text](javascript:…)` never becomes one
   * of these at all, it stays the characters somebody typed. That is the first of two fences — the
   * second is the opener plugin's own scope, which admits `http`, `https` and `mailto` and refuses
   * everything else. Neither is meant to be load-bearing alone.
   */
  | { kind: "link"; text: string; href: string };

/** One line of prose, and what it is. */
export type Line =
  | { kind: "heading"; level: number; spans: Span[] }
  /**
   * One item of a list.
   *
   * `marker` is the author's own — `1.`, `7)`, or `null` for a dash or a star. Never renumbered: a
   * model that writes 1, 2, 2, 3 wrote that, and a renderer that silently corrected it would be
   * editing the answer.
   *
   * `depth` is how deeply it is nested, counted from the indents this list actually uses rather
   * than from a fixed number of spaces — see `lines`.
   */
  | { kind: "bullet"; depth: number; marker: string | null; spans: Span[] }
  | { kind: "quote"; spans: Span[] }
  | { kind: "rule" }
  /** A blank line the author typed. It is the paragraph break, and it has to take up room. */
  | { kind: "blank" }
  | { kind: "line"; spans: Span[] };

const FENCE = /^\s*```(.*)$/;

/** A row of a table: pipes at both ends, once the line is trimmed. */
const TABLE_ROW = /^\s*\|.*\|\s*$/;

/** The row under a table's head: pipes, dashes, and optional colons for alignment. */
const TABLE_RULE = /^\s*\|(?:\s*:?-+:?\s*\|)+\s*$/;

/**
 * Prose, fenced blocks and tables, in the order they were written.
 *
 * A fence nobody closed runs to the end rather than being read back as prose. That is the live
 * case, not an edge one: a turn polled while it writes is a half-finished answer, and an opener
 * with no closer is the normal state of one for as long as the block is being typed.
 *
 * A table is only a table where the second line says so. `| this | that |` on its own is a line of
 * prose that happens to contain pipes — a shell pipeline, an or-pattern, an ASCII sketch — and
 * demanding the separator row is what keeps those out.
 */
export function blocks(text: string): Block[] {
  const out: Block[] = [];
  const source = text.split("\n");
  let prose: string[] = [];
  let code: string[] | null = null;
  let lang: string | null = null;

  const flushProse = () => {
    const joined = prose.join("\n").trim();
    if (joined !== "") out.push({ kind: "prose", text: joined });
    prose = [];
  };

  for (let at = 0; at < source.length; at += 1) {
    const line = source[at];
    const fence = FENCE.exec(line);
    if (fence !== null && code === null) {
      flushProse();
      code = [];
      lang = fence[1].trim() === "" ? null : fence[1].trim();
      continue;
    }
    if (fence !== null && code !== null) {
      out.push({ kind: "code", text: code.join("\n"), lang });
      code = null;
      lang = null;
      continue;
    }
    if (code !== null) {
      code.push(line);
      continue;
    }

    if (
      TABLE_ROW.test(line) &&
      at + 1 < source.length &&
      TABLE_RULE.test(source[at + 1])
    ) {
      flushProse();
      const head = cells(line);
      const align = alignments(source[at + 1]);
      const rows: string[][] = [];
      let cursor = at + 2;
      while (cursor < source.length && TABLE_ROW.test(source[cursor])) {
        rows.push(cells(source[cursor]));
        cursor += 1;
      }
      out.push({ kind: "table", head, rows, align });
      at = cursor - 1;
      continue;
    }

    prose.push(line);
  }

  if (code !== null) out.push({ kind: "code", text: code.join("\n"), lang });
  else flushProse();

  return out;
}

/**
 * One table row, split on its pipes.
 *
 * An escaped pipe stays a pipe rather than splitting the cell — `\|` is how a table says the
 * character, and a column that broke in two because a cell mentioned one would be a table saying
 * something its author did not.
 */
function cells(row: string): string[] {
  const inner = row.trim().replace(/^\|/, "").replace(/\|$/, "");
  const out: string[] = [];
  let cell = "";
  for (let at = 0; at < inner.length; at += 1) {
    if (inner[at] === "\\" && inner[at + 1] === "|") {
      cell += "|";
      at += 1;
      continue;
    }
    if (inner[at] === "|") {
      out.push(cell.trim());
      cell = "";
      continue;
    }
    cell += inner[at];
  }
  out.push(cell.trim());
  return out;
}

/** How each column is set, read off the separator row's colons. */
function alignments(rule: string): Align[] {
  return cells(rule).map((cell) => {
    const left = cell.startsWith(":");
    const right = cell.endsWith(":");
    if (left && right) return "center";
    if (right) return "right";
    if (left) return "left";
    return null;
  });
}

/** A tab is four columns here, so an indent counts the same whichever key was pressed. */
function indentOf(raw: string): number {
  const lead = /^[ \t]*/.exec(raw)?.[0] ?? "";
  return lead.replace(/\t/g, "    ").length;
}

const HEADING = /^(#{1,6})\s+(.*)$/;
const UNORDERED = /^[ \t]*([-*+])\s+(.*)$/;
const ORDERED = /^[ \t]*(\d{1,9}[.)])\s+(.*)$/;
const QUOTE = /^[ \t]*>\s?(.*)$/;
const RULE = /^[ \t]*(?:-{3,}|\*{3,}|_{3,})[ \t]*$/;

/**
 * The lines of one prose block, each said to be what it is.
 *
 * Nesting is counted from the indents this list actually uses, not from a fixed number of spaces.
 * Two spaces per level and four are both ordinary, and a rule that fixed on one would read a
 * four-space author's first level as a second — so a stack of the indents seen so far decides, and
 * an item indented further than the one above it is a level deeper than it, whatever the distance.
 * The stack is dropped at anything that is not a list item or a blank line, because that is where a
 * list ends.
 *
 * Blank lines are kept as their own `blank`: they are the paragraph breaks somebody typed, and a
 * renderer that dropped them runs two thoughts together. That was always the intent here, and for a
 * while the renderer dropped them anyway — an empty paragraph has no height.
 */
export function lines(text: string): Line[] {
  const out: Line[] = [];
  // The indents this list is built out of, shallowest first. `-1` is the floor, so a top-level
  // item — indent 0 — needs no special case.
  let nesting: number[] = [-1];

  for (const raw of text.split("\n")) {
    if (raw.trim() === "") {
      // A blank line inside a list does not end it: a list with air between its items is still one
      // list, and re-flattening after every gap is how a nested item loses its level.
      out.push({ kind: "blank" });
      continue;
    }

    const heading = HEADING.exec(raw);
    if (heading !== null) {
      nesting = [-1];
      out.push({
        kind: "heading",
        level: heading[1].length,
        spans: spans(heading[2]),
      });
      continue;
    }

    if (RULE.test(raw)) {
      nesting = [-1];
      out.push({ kind: "rule" });
      continue;
    }

    const quote = QUOTE.exec(raw);
    if (quote !== null) {
      nesting = [-1];
      out.push({ kind: "quote", spans: spans(quote[1]) });
      continue;
    }

    const ordered = ORDERED.exec(raw);
    const unordered = ordered === null ? UNORDERED.exec(raw) : null;
    if (ordered !== null || unordered !== null) {
      const indent = indentOf(raw);
      while (nesting.length > 1 && nesting[nesting.length - 1] >= indent) {
        nesting.pop();
      }
      if (indent > nesting[nesting.length - 1]) nesting.push(indent);
      out.push({
        kind: "bullet",
        depth: Math.max(0, nesting.length - 2),
        marker: ordered !== null ? ordered[1] : null,
        spans: spans(
          ordered !== null ? ordered[2] : (unordered as RegExpExecArray)[2],
        ),
      });
      continue;
    }

    nesting = [-1];
    out.push({ kind: "line", spans: spans(raw) });
  }

  return out;
}

/**
 * Whether a `*` or `_` can open or close emphasis where it stands.
 *
 * CommonMark's flanking rule, cut to what matters here: an opener has something other than a space
 * after it, a closer has something other than a space before it, and `_` additionally refuses to
 * work between two word characters. That last clause is the whole reason this function exists —
 * without it `budget_usd` and `year_start … year_end` become italics, and those are not prose, they
 * are the names this app is full of.
 */
function flanks(
  text: string,
  at: number,
  run: number,
  opening: boolean,
): boolean {
  const before = at > 0 ? text[at - 1] : "";
  const after = at + run < text.length ? text[at + run] : "";
  const side = opening ? after : before;
  if (side === "" || /\s/.test(side)) return false;
  // A word on both sides means this is part of a name, not a delimiter.
  if (text[at] === "_" && /\w/.test(before) && /\w/.test(after)) return false;
  return true;
}

/** A link at `at`, or null. The scheme is checked here and nowhere else — see the `link` span. */
function linkAt(text: string, at: number): { span: Span; end: number } | null {
  if (text[at] !== "[") return null;
  const close = text.indexOf("]", at + 1);
  if (close === -1 || text[close + 1] !== "(") return null;
  const end = text.indexOf(")", close + 2);
  if (end === -1) return null;
  const label = text.slice(at + 1, close);
  const href = text.slice(close + 2, end).trim();
  if (label === "" || href === "") return null;
  // Only the schemes the window can actually hand to the OS. Anything else — `javascript:`,
  // `data:`, `file:`, a bare word — stays the characters somebody typed, which is both safe and
  // honest: the reader sees the URL rather than a control that would do nothing, or worse.
  if (!/^(?:https?|mailto):/i.test(href)) return null;
  return { span: { kind: "link", text: label, href }, end: end + 1 };
}

/**
 * One line, split into plain words, inline code, bold, italic and links.
 *
 * An opener with no closer stays the character it is. A stray backtick is a price, a half-typed
 * command, a tick somebody meant literally — and swallowing the rest of the line into a code span
 * because of one is a bigger lie than showing it. The same goes for a lone `**`, a `[` that opens
 * nothing, and an underscore in the middle of a name.
 *
 * Flat, deliberately: a span carries text and never other spans, so bold with a code chip inside it
 * comes out bold with visible backticks rather than nested. Nesting is the one shape left out,
 * because every version of it costs a parser that recurses into text it does not control, and the
 * answers this draws do not lean on it.
 */
export function spans(text: string): Span[] {
  const out: Span[] = [];
  let plain = "";

  const keep = () => {
    if (plain !== "") out.push({ kind: "plain", text: plain });
    plain = "";
  };

  let at = 0;
  while (at < text.length) {
    const ch = text[at];

    if (ch === "`") {
      const close = text.indexOf("`", at + 1);
      if (close !== -1) {
        keep();
        out.push({ kind: "code", text: text.slice(at + 1, close) });
        at = close + 1;
        continue;
      }
    }

    if (ch === "[") {
      const link = linkAt(text, at);
      if (link !== null) {
        keep();
        out.push(link.span);
        at = link.end;
        continue;
      }
    }

    if (ch === "*" && text[at + 1] === "*") {
      const close = text.indexOf("**", at + 2);
      if (
        close !== -1 &&
        close > at + 2 &&
        flanks(text, at, 2, true) &&
        flanks(text, close, 2, false)
      ) {
        keep();
        out.push({ kind: "strong", text: text.slice(at + 2, close) });
        at = close + 2;
        continue;
      }
    }

    if ((ch === "*" || ch === "_") && text[at + 1] !== ch) {
      const close = text.indexOf(ch, at + 1);
      if (
        close !== -1 &&
        close > at + 1 &&
        flanks(text, at, 1, true) &&
        flanks(text, close, 1, false)
      ) {
        keep();
        out.push({ kind: "em", text: text.slice(at + 1, close) });
        at = close + 1;
        continue;
      }
    }

    plain += ch;
    at += 1;
  }

  keep();
  return out;
}
