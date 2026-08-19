/**
 * What a model writes, parsed into the handful of shapes it actually uses.
 *
 * Not a markdown library, and deliberately not: this app draws text that came out of somebody
 * else's transcript, and the rule everywhere it does that is *text, never markup*. So nothing here
 * emits HTML or accepts it — the parser returns data, the component decides what an element is, and
 * a transcript containing `<script>` is a string containing `<script>` at every step.
 *
 * The closed set is fenced code, inline code, bold, headings and bullets. Everything outside it
 * stays the characters it was: a chat answer leans on those five and on nothing else often enough
 * to matter, and each shape added is another way to mangle text that was fine as it was.
 *
 * Kept beside `turns.ts` for the same reason that one is: shape, not fetching, and testable without
 * mounting anything.
 */

/** A run of prose, or one fenced code block. */
export type Block =
  | { kind: "prose"; text: string }
  | { kind: "code"; text: string; lang: string | null };

/** A piece of one line of prose. */
export type Span =
  | { kind: "plain"; text: string }
  | { kind: "code"; text: string }
  | { kind: "strong"; text: string };

/** One line of prose, and what it is. */
export type Line =
  | { kind: "heading"; level: number; spans: Span[] }
  | { kind: "bullet"; spans: Span[] }
  | { kind: "line"; spans: Span[] };

const FENCE = /^\s*```(.*)$/;

/**
 * Prose and fenced blocks, in the order they were written.
 *
 * A fence nobody closed runs to the end rather than being read back as prose. That is the live
 * case, not an edge one: a turn polled while it writes is a half-finished answer, and an opener
 * with no closer is the normal state of one for as long as the block is being typed.
 */
export function blocks(text: string): Block[] {
  const out: Block[] = [];
  let prose: string[] = [];
  let code: string[] | null = null;
  let lang: string | null = null;

  const flushProse = () => {
    const joined = prose.join("\n").trim();
    if (joined !== "") out.push({ kind: "prose", text: joined });
    prose = [];
  };

  for (const line of text.split("\n")) {
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
    if (code !== null) code.push(line);
    else prose.push(line);
  }

  if (code !== null) out.push({ kind: "code", text: code.join("\n"), lang });
  else flushProse();

  return out;
}

/**
 * The lines of one prose block, each said to be a heading, a bullet or neither.
 *
 * Blank lines are kept as their own `line` with no spans: they are the paragraph breaks somebody
 * typed, and a renderer that dropped them would run two thoughts together.
 */
export function lines(text: string): Line[] {
  return text.split("\n").map((raw) => {
    const heading = /^(#{1,4})\s+(.*)$/.exec(raw);
    if (heading !== null) {
      return { kind: "heading", level: heading[1].length, spans: spans(heading[2]) };
    }
    const bullet = /^\s*[-*]\s+(.*)$/.exec(raw);
    if (bullet !== null) return { kind: "bullet", spans: spans(bullet[1]) };
    return { kind: "line", spans: spans(raw) };
  });
}

/**
 * One line, split into plain words, inline code and bold.
 *
 * An opener with no closer stays the character it is. A stray backtick is a price, a half-typed
 * command, a tick somebody meant literally — and swallowing the rest of the line into a code span
 * because of one is a bigger lie than showing it.
 */
export function spans(text: string): Span[] {
  const out: Span[] = [];
  let plain = "";
  let at = 0;

  const keep = () => {
    if (plain !== "") out.push({ kind: "plain", text: plain });
    plain = "";
  };

  while (at < text.length) {
    const tick = text.indexOf("`", at);
    const star = text.indexOf("**", at);

    if (tick !== -1 && (star === -1 || tick < star)) {
      const close = text.indexOf("`", tick + 1);
      if (close === -1) break;
      plain += text.slice(at, tick);
      keep();
      out.push({ kind: "code", text: text.slice(tick + 1, close) });
      at = close + 1;
      continue;
    }

    if (star !== -1) {
      const close = text.indexOf("**", star + 2);
      if (close === -1) break;
      plain += text.slice(at, star);
      keep();
      out.push({ kind: "strong", text: text.slice(star + 2, close) });
      at = close + 2;
      continue;
    }

    break;
  }

  plain += text.slice(at);
  keep();
  return out;
}
