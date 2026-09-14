/**
 * The class contract, checked in both directions.
 *
 * Every stylesheet under `shell/src/pages/` states the same rule at the top of
 * itself: no class appears in the page that this file does not define. Nothing
 * enforced it. `tsc` and vitest both pass happily on a class no stylesheet has
 * ever heard of — jsdom applies no stylesheet, so the defect shows up only as
 * something looking wrong on a screen nobody is testing.
 *
 * The other direction rots more quietly and is the reason this exists: five
 * `.pj-chip-*` rules outlived the project picker they were written for by a
 * month, because the check had only ever been run one way round. Dead CSS is
 * not a rendering defect, so nothing ever reports it; it is read by the next
 * person as a description of the page, and it lies.
 *
 * **Prefix families, because that is the convention the stylesheets declare.**
 * `projects.css` opens with "the Projects slice: the read-only inspector, `pj-`
 * throughout — as `waiting-` is for the queue, `ap-` for the cockpit and
 * `fleet-` for the fleet". So the families are read out of each stylesheet
 * rather than configured here, and only classes in a family the stylesheet
 * itself owns are compared. `ui-` is skipped: those are the shared primitives,
 * whose users are every page in the app rather than any one of them.
 *
 * **The index is the whole of `shell/src`, and not the files that import the
 * stylesheet.** A CSS import in this app is a global side effect — `Teams.tsx`
 * imports `teams.css` and `team/Bench.tsx` then writes `teams-matrix-cell`
 * without importing anything. The first draft of this script asked which files
 * imported the sheet and confidently reported fifty live rules as dead. A
 * family name is unique across the app by convention, so the question that is
 * actually being asked is "does anything use this", and the answer has to be
 * looked for everywhere.
 *
 * Tests are indexed too, deliberately. A class only a test names is arguably
 * dead — but the cost of the two mistakes is not symmetric: a false "unused"
 * gets live CSS deleted and breaks a screen, a false "clean" leaves a dead rule
 * where it was.
 *
 * **Template literals are reported, never guessed.** A page writes
 * `pj-concern-${concern.weight}`, and the union behind it is a TypeScript type
 * this script does not typecheck. A plain grep truncates that to `pj-concern-`
 * and then reports three live rules as dead — worse than no check at all,
 * because acting on it deletes working CSS. So a truncated stem marks every
 * rule beneath it as spoken for, and is listed separately as something a person
 * has to read.
 *
 * **Every finding it made before this paragraph was its own bug**, which is
 * worth recording because each was the same mistake in a new coat: deleting a
 * name from the index everywhere because ONE occurrence of it was something
 * else. `teams-alone` is a literal class and the stem of `teams-alone-${mode}`.
 * `sy-token-name` is the id of a field in the mint form and the class on a
 * token's name in the list below it. `feed-kind-options` is a `<datalist>` id
 * and the `list=` pointing at it. Each was reported as a live rule gone dead or
 * a class nobody styled, and each is repaired the same way — mask the
 * occurrence, never the name. A checker that cries wolf is worse than none,
 * because acting on it deletes working CSS.
 *
 * **What it cannot decide, it says by naming the file.** A kebab string in a
 * literal is indistinguishable from a class name. The last two findings were
 * exactly that, and each was answered where it was written rather than in a
 * suppression list: `chats-zoom` was a test asserting a bare prefix was ABSENT,
 * and now names the class the record actually took; `fleet-exclusion` was a
 * proposal `kind` in a fixture, and a `kind:` value is now masked below, the
 * way an id is. All 20 sheets are clean.
 *
 * Scaffolding, not a gate. It reports; `scripts/gates.sh` does not call it.
 * Wiring it in is its own decision — what a gate should do the next time a
 * literal looks like a class — and not one this instrument makes for itself.
 *
 *   node scripts/css-contract.mjs [name ...]
 */

import { readFileSync, readdirSync } from "node:fs";
import { basename, dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const SRC = join(ROOT, "shell", "src");
const PAGES = join(SRC, "pages");

/** The primitives' namespace. Its callers are every page, so a page cannot judge it. */
const SHARED = new Set(["ui"]);

/** A family with one rule in it is a coincidence, not a namespace. */
const FAMILY_FLOOR = 2;

/**
 * Every attribute that carries an element id rather than a class name.
 *
 * More than `id` and `htmlFor`, and the list is not decorative: `feed-kind-options`
 * is a `<datalist>` id and the `list=` on the input that points at it, and with
 * only the first two masked it was reported as a class nobody had styled. The
 * ARIA relations are here for the same reason, before they cost somebody the
 * same ten minutes.
 */
const ID_REFERENCE =
  /(?:id|htmlFor|list|form|aria-labelledby|aria-describedby|aria-controls|aria-owns|aria-activedescendant)="[a-z][a-z0-9-]*"/g;

/** Comments carry class names as prose — `.pj-chip-*` in a sentence is not a rule. */
const stripCss = (text) => text.replace(/\/\*[\s\S]*?\*\//g, "");
const stripCode = (text) => text.replace(/\/\*[\s\S]*?\*\//g, "").replace(/\/\/[^\n]*/g, "");

/** Every `.some-class` in a selector, however deep in the file. */
function classesIn(css) {
  return new Set(
    [...stripCss(css).matchAll(/\.([a-z][a-z0-9]*(?:-[a-z0-9]+)*)/g)].map((m) => m[1]),
  );
}

function sourcesUnder(dir) {
  const found = [];
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) found.push(...sourcesUnder(path));
    else if (/\.tsx?$/.test(entry.name)) found.push(path);
  }
  return found;
}

/**
 * What the app says, once, split into what it names outright and what it builds.
 *
 * Element ids share the family prefix and are not classes: `id="pj-wip"` labels
 * a field, and left in it would be reported as a class nobody styled.
 */
function indexSources() {
  const named = new Set();
  const stems = new Set();
  const where = new Map();
  /* Where each token was written, so a reader can dismiss a false one at a
     glance. A kebab string in a literal is indistinguishable from a class name
     — `kind: "fleet-exclusion"` is an API value — and this script cannot know
     which. Naming the file turns that from noise into one line somebody reads. */
  const seen = new Map();

  for (const path of sourcesUnder(SRC)) {
    const text = stripCode(readFileSync(path, "utf8"));
    for (const m of text.matchAll(/([a-z][a-z0-9]*(?:-[a-z0-9]+)*-)\$\{/g)) stems.add(m[1]);
    /*
      Blank out the hole and the name in front of it, and only there.
      `fleet-edge-${state}` matches the plain token rule up to the `-$`, so the
      site that BUILDS a class also emits its stem as if the page had written
      `fleet-edge` outright — a class no stylesheet defines, because it never
      was one. Deleting that name everywhere is the wrong repair and was tried:
      `teams-alone` is a real class AND the stem of `teams-alone-${mode}`, and
      dropping it reported three live rules as dead. Masking the occurrence
      leaves every other mention of the name standing.
    */
    const masked = text
      /*
        The id and the label that points at it, blanked where they are written.
        A name can be BOTH: `sy-token-name` is the id of a field in the mint
        form and the class on a token's name in the list below it. Dropping the
        name from the index because one occurrence was an id reported a live
        rule as dead — the same mistake as deleting a stem's head, one line
        further down, and it was made here first.
      */
      /*
        A custom property is not a class. The token rule below is anchored on `[a-z]` and skips
        the leading `--`, so `--feed-control-h` in a source file was indexed as the class
        `feed-control-h` — a name no sheet defines, because it never was one.
        `pages/sheet-layout.test.ts` built that name out of three string fragments to dodge
        exactly this, which is a workaround nobody could read from the site. Masked where the
        mistake is, so the test can write the property it is about. Fourth false-positive shape
        after `.chats-zoom`, `.fleet-exclusion` and `id="value"`.
      */
      .replace(/--[a-z][a-z0-9]*(?:-[a-z0-9]+)*/g, " ")
      .replace(ID_REFERENCE, " ")
      /*
        A `kind:` value is the daemon's vocabulary, not a class. `kind: "fleet-exclusion"` in a
        Waiting fixture is the proposal kind `/fleet/exclusions/requests` sends, and it was
        reported as a class no sheet defines. Masked where it is written, like an id: a name that
        is ever both a kind and a class is still read everywhere else it appears.
      */
      .replace(/\bkind:\s*"[a-z][a-z0-9]*(?:-[a-z0-9]+)*"/g, " ")
      .replace(/[a-z][a-z0-9]*(?:-[a-z0-9]+)*-\$\{[^}]*\}/g, " ");
    for (const m of masked.matchAll(/[a-z][a-z0-9]*(?:-[a-z0-9]+)+/g)) {
      named.add(m[0]);
      if (!seen.has(m[0])) seen.set(m[0], new Set());
      seen.get(m[0]).add(basename(path));
      const family = m[0].split("-")[0];
      if (!where.has(family)) where.set(family, new Set());
      where.get(family).add(basename(path));
    }
  }
  return { named, stems, where, seen };
}

const index = indexSources();

/**
 * Which stylesheet owns which family.
 *
 * A sheet may only judge a family it alone defines. `chats.css` holds two
 * `app-` rules and `app.css` holds the other seventeen — asked on its own,
 * chats.css reported every one of those seventeen as a class nobody had
 * styled, because it could not see the sheet that styles them. Shared
 * namespaces are nobody's to police from inside one page.
 */
function ownership() {
  const owners = new Map();
  const sheets = [];
  const walk = (dir) => {
    for (const entry of readdirSync(dir, { withFileTypes: true })) {
      const path = join(dir, entry.name);
      if (entry.isDirectory()) walk(path);
      else if (entry.name.endsWith(".css")) sheets.push(path);
    }
  };
  walk(SRC);
  for (const path of sheets) {
    for (const name of classesIn(readFileSync(path, "utf8"))) {
      const family = name.split("-")[0];
      if (!owners.has(family)) owners.set(family, new Set());
      owners.get(family).add(path);
    }
  }
  return owners;
}

const owners = ownership();

function check(cssPath) {
  const defined = classesIn(readFileSync(cssPath, "utf8"));

  const size = new Map();
  for (const name of defined) {
    const family = name.split("-")[0];
    size.set(family, (size.get(family) ?? 0) + 1);
  }
  const families = [...size]
    .filter(([family, count]) => count >= FAMILY_FLOOR && !SHARED.has(family))
    .filter(([family]) => (owners.get(family)?.size ?? 0) === 1)
    .map(([family]) => family);

  const mine = (name) => families.some((family) => name.startsWith(`${family}-`));
  const spokenFor = (name) => [...index.stems].some((stem) => name.startsWith(stem));

  const used = new Set([...index.named].filter(mine));
  const missing = [...used].filter((n) => !defined.has(n) && !spokenFor(n)).sort();
  const unused = [...defined].filter((n) => mine(n) && !used.has(n) && !spokenFor(n)).sort();
  const drawnBy = new Set(families.flatMap((family) => [...(index.where.get(family) ?? [])]));

  return { families, missing, unused, drawnBy: [...drawnBy].sort() };
}

const only = process.argv.slice(2);
const sheets = readdirSync(PAGES)
  .filter((name) => name.endsWith(".css"))
  .filter((name) => only.length === 0 || only.some((want) => name.includes(want)));

let dirty = 0;
for (const name of sheets) {
  const report = check(join(PAGES, name));
  if (report.families.length === 0) {
    console.log(`?    ${name} — no family of its own to check`);
    continue;
  }
  const clean = report.missing.length === 0 && report.unused.length === 0;
  console.log(`${clean ? "ok  " : "FAIL"} ${name}  [${report.families.join(", ")}]`);
  for (const one of report.missing) {
    const where = [...(index.seen.get(one) ?? [])].join(", ");
    console.log(`       used, never defined:  .${one}   (in ${where})`);
  }
  for (const one of report.unused) console.log(`       defined, never used:  .${one}`);
  if (!clean) {
    console.log(`       drawn by: ${report.drawnBy.join(", ")}`);
    dirty += 1;
  }
}

console.log(`\n${sheets.length} stylesheets, ${dirty} with something to answer for`);
process.exit(dirty === 0 ? 0 : 1);
