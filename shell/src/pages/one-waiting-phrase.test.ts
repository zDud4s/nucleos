import { readdirSync, readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

/**
 * One arithmetic owns the phrase "waiting on you": the Waiting page's own six
 * decision lists, shown on Home and on the rail badge. Every other surface
 * names the noun it actually counts — proposals, team actions, runs awaiting
 * approval, notes proposed. Eight pages once said "waiting on you" about
 * eight different numbers, and a reader had no way to tell which one was the
 * queue. This test is the fence: a new page that borrows the phrase fails here
 * rather than in a screenshot nobody reads.
 *
 * The Waiting page is Waiting.tsx plus its module directory waiting/. It keeps
 * one match on purpose, scoped by the word "git" (an `aria-label` that
 * `getByRole("list", { name })` depends on), so the rule for those files is
 * "every match also says git". Other page sub-directories (files/, ...) are
 * scanned too, so a split page cannot hide the phrase from the fence.
 */

const moduleUrl = import.meta.url.startsWith("file:")
  ? import.meta.url
  : `file://${import.meta.url}`;
const repoRoot = fileURLToPath(new URL("../../../", moduleUrl));
const pagesDir = `${repoRoot}shell/src/pages`;

const PHRASE = /waiting on you/i;

function pageSources(): Map<string, string> {
  const names = (readdirSync(pagesDir, { recursive: true }) as string[])
    .map((name) => name.split("\\").join("/"))
    .filter((name) => name.endsWith(".tsx") && !name.endsWith(".test.tsx"));
  expect(names.length, `no pages found under ${pagesDir}`).toBeGreaterThan(0);
  return new Map(
    names.map((name) => [name, readFileSync(`${pagesDir}/${name}`, "utf8")]),
  );
}

describe("the bare phrase 'waiting on you'", () => {
  it("one page owns the bare phrase", () => {
    const sources = pageSources();
    const isWaitingPage = (name: string) =>
      name === "Waiting.tsx" || name.startsWith("waiting/");
    const matching = [...sources]
      .filter(([, source]) =>
        source.split(/\r?\n/).some((line) => PHRASE.test(line)),
      )
      .map(([name]) => (isWaitingPage(name) ? "Waiting page" : name))
      .filter((name, i, all) => all.indexOf(name) === i)
      .sort();

    expect(matching).toEqual(["Home.tsx", "Waiting page"]);

    const home = sources.get("Home.tsx")!;
    expect(
      home.split(/\r?\n/).filter((line) => PHRASE.test(line)).length,
    ).toBeGreaterThan(0);

    const waitingLines = [...sources]
      .filter(([name]) => isWaitingPage(name))
      .flatMap(([, source]) => source.split(/\r?\n/))
      .filter((line) => PHRASE.test(line));
    expect(waitingLines.length).toBeGreaterThan(0);
    for (const line of waitingLines) {
      expect(line, `unscoped phrase on the Waiting page: ${line.trim()}`).toMatch(
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
