import { readdirSync, readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

/**
 * One arithmetic owns the phrase "waiting on you": the Waiting page's own six
 * decision lists, shown on Home and on the rail badge. Every other surface
 * names the noun it actually counts — proposals, team actions, runs awaiting
 * approval, refinements proposed. Eight pages once said "waiting on you" about
 * eight different numbers, and a reader had no way to tell which one was the
 * queue. This test is the fence: a new page that borrows the phrase fails here
 * rather than in a screenshot nobody reads.
 *
 * Waiting.tsx keeps two matches on purpose, both scoped by the word "git" (an
 * empty-state sentence and an `aria-label` that `getByRole("list", { name })`
 * depends on), so the rule for that file is "every match also says git".
 */

const moduleUrl = import.meta.url.startsWith("file:")
  ? import.meta.url
  : `file://${import.meta.url}`;
const repoRoot = fileURLToPath(new URL("../../../", moduleUrl));
const pagesDir = `${repoRoot}shell/src/pages`;

const PHRASE = /waiting on you/i;

function pageSources(): Map<string, string> {
  const names = readdirSync(pagesDir).filter(
    (name) => name.endsWith(".tsx") && !name.endsWith(".test.tsx"),
  );
  expect(names.length, `no pages found under ${pagesDir}`).toBeGreaterThan(0);
  return new Map(
    names.map((name) => [name, readFileSync(`${pagesDir}/${name}`, "utf8")]),
  );
}

describe("the bare phrase 'waiting on you'", () => {
  it("one page owns the bare phrase", () => {
    const sources = pageSources();
    const matching = [...sources]
      .filter(([, source]) =>
        source.split(/\r?\n/).some((line) => PHRASE.test(line)),
      )
      .map(([name]) => name)
      .sort();

    expect(matching).toEqual(["Home.tsx", "Waiting.tsx"]);

    const home = sources.get("Home.tsx")!;
    expect(
      home.split(/\r?\n/).filter((line) => PHRASE.test(line)).length,
    ).toBeGreaterThan(0);

    const waitingLines = sources
      .get("Waiting.tsx")!
      .split(/\r?\n/)
      .filter((line) => PHRASE.test(line));
    expect(waitingLines.length).toBeGreaterThan(0);
    for (const line of waitingLines) {
      expect(line, `unscoped phrase in Waiting.tsx: ${line.trim()}`).toMatch(
        /git/i,
      );
    }
  });

  it("the rail badge is fed by the one count", () => {
    const appShell = readFileSync(
      `${repoRoot}shell/src/app/AppShell.tsx`,
      "utf8",
    );
    expect(appShell).toContain("useWaitingCount(");
    expect(appShell).not.toContain("proposals.data?.length");
  });
});
