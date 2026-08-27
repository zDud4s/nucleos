import type { Anchored, Junction } from "../data/project-map";

/**
 * The junction: the nodes that do not match.
 *
 * Decision 3 of the spec — *"o valor está na junção: os nós que interessam são os que não
 * casam"* — is the whole reason the structure layer exists, and this is the panel that says it
 * out loud. It leads with what nothing in the project claims, because that is the only answer
 * this map is **certain** about today and it is exactly §5.1's *declared, with no code*.
 *
 * **The asymmetry is the design, and this surface has to carry it.** Not one citation in this
 * repository says which document its `§` belongs to (§8), so every *positive* join the núcleo can
 * make is a guess and comes back `ambiguous`. The negatives are untouched by that: a section no
 * file names anywhere is claimed under no document, whichever document each bare `§` meant. So
 * the mismatches lead, and no ambiguous line is ever drawn as a confirmation — a map that is
 * confidently wrong is the disease this feature treats, with better pixels.
 *
 * **Pure and presentational, fed by the query `ModeMapa` already holds.** It issues no request,
 * so it cannot fail on its own and has no loading state to draw. Reading the map a second time
 * here would put two answers to one question on one screen, free to disagree.
 *
 * **No control, and the absence is deliberate.** This is a reading surface. The one place in this
 * mode that has buttons answers a single line at a time, on purpose, and a second place to press
 * would be a second way to accept without reading. Nothing here implies a verdict either: a
 * verdict is the owner's alone, and the surface that carries one is a later slice.
 *
 * **No percentage, no single score, no bar** (§12). A collapsed number is exactly the collapse §5
 * forbids, and it is also the shape that manufactures the false confidence in §1: one figure that
 * looks like an answer and was never checked against anything.
 */

export interface JuncaoProps {
  junction: Junction;
}

/**
 * How many rows of a pile are drawn before it is summarised.
 *
 * A hundred paths on screen is the thousand-line plan again, which is the thing this mode exists
 * to replace — but a list that quietly stops is the same defect, so whatever is cut is counted
 * out loud beside it. The number never leaves the screen; only the rows do.
 */
const ROWS = 12;

/** The first `limit` of a pile, and how many were left out. */
function capped<T>(all: T[], limit = ROWS): { shown: T[]; hidden: number } {
  return { shown: all.slice(0, limit), hidden: Math.max(0, all.length - limit) };
}

function plural(count: number, one: string, many: string): string {
  return count === 1 ? one : many;
}

export function Juncao({ junction }: JuncaoProps) {
  const { decisions, unclaimed, counts } = junction;

  const silent = decisions.filter((row) => row.anchor === "silent");
  const unnumbered = decisions.filter((row) => row.anchor === "unnumbered");
  /*
    The two uncertainties `ambiguous` carries, told apart the only way they can be — by `modules`
    against `foreign`, never by the anchor itself. A line named by a readable module is *which
    document did this mean?*; a line named only abroad is *this is code I cannot read*. `join`
    guarantees an ambiguous line has at least one of the two, so these two lists partition it.
  */
  const unattributed = decisions.filter(
    (row) => row.anchor === "ambiguous" && row.modules.length > 0,
  );
  const abroad = decisions.filter(
    (row) => row.anchor === "ambiguous" && row.modules.length === 0 && row.foreign.length > 0,
  );

  return (
    <section aria-label="The junction" className="flex flex-col gap-6">
      <h2 className="font-display text-xs font-medium uppercase tracking-wider text-text-faint">
        What does not match
      </h2>

      {counts.decisions === 0 ? <NoIntention /> : <Silent rows={silent} />}
      {unnumbered.length > 0 ? <Unnumbered rows={unnumbered} /> : null}

      <Unclaimed paths={unclaimed} />

      {counts.decisions === 0 ? null : (
        <Plausible total={counts.ambiguous} unattributed={unattributed} abroad={abroad} />
      )}

      <Missing declared={counts.declared} decisions={counts.decisions} />
    </section>
  );
}

/**
 * §11: a project with no specs, answered in words.
 *
 * The structure layer above is real and costs nothing. The layer that would say whether it
 * matches what was decided does not exist yet, and this says so along with what would make one —
 * "uma tela vazia seria a mentira mais barata do documento".
 *
 * **Deliberately not a grid of zeros.** A row of `0`s reads as a measurement, and here nothing
 * has been measured: no decision was checked and found unclaimed, because there is no decision.
 * That is the §1 failure in miniature — a figure that looks like an answer and is the absence of
 * one.
 */
function NoIntention() {
  return (
    <div className="flex flex-col gap-2">
      <p className="max-w-prose text-sm text-text-muted">
        No decision has been approved for this project yet, so there is nothing to read the
        structure against. Every number this panel could show would be a zero, and a zero here
        would read as a measurement of something that was never measured.
      </p>
      <p className="max-w-prose text-sm text-text-muted">
        The structure is derived from the code and is always true. The layer that says whether it
        matches what you decided is made one line at a time: extract a spec below, then answer the
        lines it proposes. Nothing enters this map without that answer.
      </p>
    </div>
  );
}

/**
 * §5.1's *declared, with no code*, and the headline of this panel.
 *
 * **The one thing here the map is certain about.** Every positive join in this repository is a
 * guess about which document a bare `§` meant; this is untouched by that, because if nothing
 * names a section at all then nothing claims it under any document. Decision 3 says the nodes
 * that matter are the ones that do not match, which makes the answers this map is sure of exactly
 * the ones it exists to give.
 *
 * The empty case is a sentence and never a blank area — the two look identical, and reading the
 * first as the second is the false confidence this mode exists to cure. It is also written so it
 * cannot be read as a pass: nothing here has compared a line of code to what a decision says.
 */
function Silent({ rows }: { rows: Anchored[] }) {
  if (rows.length === 0) {
    return (
      <p className="max-w-prose text-sm text-text-muted">
        No approved decision is declared with no code right now — every section that could be
        looked for was named by something. That is not a pass: how firmly it is named is below,
        and none of it has read a line of code against what a decision actually says.
      </p>
    );
  }

  const { shown, hidden } = capped(rows);

  return (
    <div className="flex flex-col gap-2">
      <p className="font-display text-3xl text-text">
        {rows.length}
        <span className="ml-2 text-sm text-text-faint">
          approved {plural(rows.length, "decision", "decisions")} that nothing in this project
          names
        </span>
      </p>
      <p className="max-w-prose text-xs text-text-muted">
        §5.1 calls this declared, with no code. It is the one answer here the map is certain
        about: a section no file names anywhere is claimed under no document, whichever document
        each bare § was meant to point at.
      </p>
      <ul aria-label="Decisions nothing names" className="flex flex-col gap-2">
        {shown.map((row) => (
          <Line key={row.decision_id} row={row} />
        ))}
      </ul>
      {hidden > 0 ? <Hidden count={hidden} /> : null}
    </div>
  );
}

/**
 * A decision whose heading carried no number, so nothing could be looked for.
 *
 * **Kept out of the pile above rather than folded into it.** *Nothing claims this* is the report
 * of a search, and here no search ran — a decision extracted from `## Contrato` is approved and
 * real, it simply anchors nothing. Showing it as having no code would be reporting a search that
 * never happened, on the one panel built to stop exactly that.
 */
function Unnumbered({ rows }: { rows: Anchored[] }) {
  const { shown, hidden } = capped(rows);

  return (
    <div className="flex flex-col gap-2">
      <p className="max-w-prose text-xs text-text-muted">
        {rows.length} approved {plural(rows.length, "line", "lines")} came from a heading with no
        number, so there was nothing to look for. They are counted apart from the pile above
        rather than folded into it — calling them code-less would report a search that never ran.
      </p>
      <ul aria-label="Decisions whose heading carried no number" className="flex flex-col gap-2">
        {shown.map((row) => (
          <Line key={row.decision_id} row={row} />
        ))}
      </ul>
      {hidden > 0 ? <Hidden count={hidden} /> : null}
    </div>
  );
}

/**
 * §5.1's *code nobody asked for*: the pile that grows inside a plan nobody read to the end.
 *
 * Read off `unclaimed` and never off `declares`. The two are near-synonyms that disagree: a file
 * holding a bare `§` with no number gestures at a section it never names, and a module whose
 * sibling test names what it proves is claimed by that test. Counting the gesture would put four
 * modules of this repository in a pile they do not belong in — a wrong answer wearing the right
 * word.
 *
 * Drawn even when no decision has been approved, because this number is derived from the code
 * alone and owes nothing to the intention layer. Hiding a real measurement behind a missing one
 * would be the blank screen §11 refuses.
 */
function Unclaimed({ paths }: { paths: string[] }) {
  if (paths.length === 0) {
    return (
      <p className="max-w-prose text-sm text-text-muted">
        Every module this reader could read names some section, so nothing is sitting in the code
        nobody asked for pile.
      </p>
    );
  }

  const { shown, hidden } = capped(paths);

  return (
    <div className="flex flex-col gap-2">
      <p className="font-display text-3xl text-text">
        {paths.length}
        <span className="ml-2 text-sm text-text-faint">
          {plural(paths.length, "module", "modules")} nobody asked for
        </span>
      </p>
      <p className="max-w-prose text-xs text-text-muted">
        They name no spec section at all, and neither does whatever tests them. §5.1 calls this
        code nobody asked for, and it is the pile that fills up inside a plan nobody read to the
        end.
      </p>
      <ul aria-label="Modules nobody asked for" className="flex flex-col gap-1">
        {shown.map((path) => (
          <li key={path} className="truncate font-mono text-xs text-text-muted">
            {path}
          </li>
        ))}
      </ul>
      {hidden > 0 ? <Hidden count={hidden} /> : null}
    </div>
  );
}

/**
 * The joins that are only plausible, in two kinds that never share a sentence.
 *
 * §8: a `§` in a file here is a number and nothing else, and which of this project's documents it
 * points at is written down nowhere in the file. So a section named by code is a *candidate* for
 * a decision and not a match — and a later slice fixes it by proposing the citation edits that
 * put a document slug on each one.
 *
 * The other kind is not the same doubt at all: the section is named only by files in a language
 * this map cannot read. Something in the sidecars points at it; nothing here can open that file
 * and say what it does with it. Two different amounts of not-knowing get two sentences, because a
 * reader handed one sentence for both has been handed the weaker of the two claims for each.
 */
function Plausible({
  total,
  unattributed,
  abroad,
}: {
  total: number;
  unattributed: Anchored[];
  abroad: Anchored[];
}) {
  if (total === 0) {
    return (
      <p className="max-w-prose text-sm text-text-muted">
        Nothing here is joined to code on a guess right now — no approved decision is named by
        code this map could not tie to it.
      </p>
    );
  }

  const named = capped(unattributed);
  const overseas = capped(abroad);

  return (
    <div className="flex flex-col gap-3">
      <p className="font-display text-3xl text-text">
        {total}
        <span className="ml-2 text-sm text-text-faint">
          {plural(total, "join that is", "joins that are")} only plausible
        </span>
      </p>

      {unattributed.length > 0 ? (
        <div className="flex flex-col gap-2">
          <p className="max-w-prose text-xs text-text-muted">
            Code names the section, and never says which document the section belongs to. A § in a
            file is a number and nothing else, so it may just as easily be another document&rsquo;s
            section of the same number. Until a citation carries a document slug, each of these is
            a guess — shown because it might be the join, and never counted as one.
          </p>
          <ul aria-label="Joins whose citation names no document" className="flex flex-col gap-2">
            {named.shown.map((row) => (
              <Line key={row.decision_id} row={row} paths={row.modules} />
            ))}
          </ul>
          {named.hidden > 0 ? <Hidden count={named.hidden} /> : null}
        </div>
      ) : null}

      {abroad.length > 0 ? (
        <div className="flex flex-col gap-2">
          <p className="max-w-prose text-xs text-text-muted">
            Named only by code this map cannot read yet — the Go sidecars. Something in there
            points at the section; nothing here can open the file and say what it does with it.
            That is a different amount of not-knowing from the one above, and it is deliberately
            not the same sentence.
          </p>
          <ul
            aria-label="Joins named only by code this map cannot read"
            className="flex flex-col gap-2"
          >
            {overseas.shown.map((row) => (
              <Line key={row.decision_id} row={row} paths={row.foreign} />
            ))}
          </ul>
          {overseas.hidden > 0 ? <Hidden count={overseas.hidden} /> : null}
        </div>
      ) : null}
    </div>
  );
}

/**
 * What this panel cannot see, said rather than left to be assumed.
 *
 * §5.1 names four derived states and two of them mean *the triager looked*. There is no triager
 * (§6), so a panel drawing four states would be inventing two — and a surface that looks complete
 * while a layer is missing is the same false confidence with better pixels.
 *
 * The second paragraph is read off the payload rather than asserted, so it stays true for any
 * project: `declared` is the only state that means certain, and it needs a citation naming its
 * own document. Today no project here has one.
 */
function Missing({ declared, decisions }: { declared: number; decisions: number }) {
  return (
    <div className="flex flex-col gap-2 border-t border-border pt-4">
      <h3 className="font-display text-xs font-medium uppercase tracking-wider text-text-faint">
        What this panel cannot see
      </h3>
      <p className="max-w-prose text-xs text-text-muted">
        §5.1 names four derived states and this panel can draw two of them. The other two — waiting
        on you, and silenced — both mean a triager looked at a node and formed an opinion about it,
        and there is no triager yet. So nothing here says whether anybody looked.
      </p>
      {decisions === 0 ? null : declared === 0 ? (
        <p className="max-w-prose text-xs text-text-muted">
          No decision here is confirmed either. That state needs a citation that names its own
          document, the shape §8 asks for, and nothing this map read carries one yet — so every
          join above is a guess, while every decision with no code is sound.
        </p>
      ) : (
        <p className="max-w-prose text-xs text-text-muted">
          {declared} {plural(declared, "decision is", "decisions are")} confirmed: a readable
          module names the section and names the document. Those are the only ones here that are
          certain, and everything else above is a guess or an absence.
        </p>
      )}
      <p className="max-w-prose text-xs text-text-muted">
        Nothing on this panel is a verdict. Whether the code is what you wanted is yours to say,
        and no count decides it.
      </p>
    </div>
  );
}

/**
 * One decision, in the document's own words.
 *
 * The heading and the document slug sit above the sentence the model pulled out, because *line 3
 * of that document* is how the owner refers to a decision after approving it. Never a summary:
 * summarising twelve lines would be the thousand-line plan again, only shorter.
 */
function Line({ row, paths = [] }: { row: Anchored; paths?: string[] }) {
  const named = capped(paths, 4);

  return (
    <li className="flex flex-col gap-1 rounded-lg border border-border bg-surface p-3">
      <div className="flex flex-wrap items-baseline gap-x-3 gap-y-1">
        <span className="text-xs text-text-muted">{row.section}</span>
        <span className="truncate font-mono text-xs text-text-faint">{row.spec_slug}</span>
        <span className="ml-auto text-xs text-text-faint">{row.kind}</span>
      </div>
      <p className="max-w-prose text-sm text-text">{row.text}</p>
      {paths.length > 0 ? (
        <p className="font-mono text-xs text-text-faint">
          {named.shown.join(" · ")}
          {named.hidden > 0 ? ` · and ${named.hidden} more not shown` : ""}
        </p>
      ) : null}
    </li>
  );
}

/**
 * What a capped list left out.
 *
 * **A silent truncation is the same defect this whole feature exists to cure**, so the rows are
 * cut and the number never is. The count above the list is always the total.
 */
function Hidden({ count }: { count: number }) {
  return (
    <p className="text-xs text-text-faint">
      {count} more not shown here. The count above is the whole pile.
    </p>
  );
}
