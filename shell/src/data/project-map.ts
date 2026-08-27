import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { apiFetch } from "./client";
import { keys } from "./keys";

/**
 * A project's structure layer, and the join the núcleo now computes over it.
 *
 * **Still not the whole map.** The evidence layer, the stamps and the triage are later slices.
 * What is here is the structure — what modules exist and what they import, derived off disk and
 * always true — plus the {@link Junction}, which reads the approved decisions against that
 * structure and says, one decision at a time, what does *not* match. Decision 3 of the spec: the
 * value is in the junction, and the nodes that matter are the ones that do not match.
 */

export type Reader = "rust" | "typescript";

/**
 * One `§` reference found in a source file.
 *
 * **`named` is a candidate for a document slug and never a document.** The word after a section
 * number has the same shape whether it is a slug or an English word, and nothing lexical tells
 * them apart: `§4.4 rule` yields `"rule"` exactly the way `§6.4 workspace-de-projeto` yields the
 * slug. The núcleo checks the candidate against the project's real spec slugs when it joins and
 * drops it when it is not one — but what arrives here, hanging off a module or a foreign file, is
 * the raw candidate that check has not been applied to. Reading a non-null `named` as *this
 * citation points at that document* is the one wrong conclusion this field invites.
 */
export interface Citation {
  /** The section label, normalized: `7`, `6.4`, `5.3a`. Never the `§`, never the punctuation after. */
  section: string;
  named: string | null;
}

export interface MapModule {
  path: string;
  reader: Reader;
  /**
   * The file's own gesture at a spec section — its source contains a `§`.
   *
   * **Not the same question as {@link MapModule.cites} being empty, and the two disagree on
   * purpose.** A file holding a bare `§` with no number is `declares: true, cites: []`; a module
   * citing nothing itself whose sibling test names `§9.2` is `declares: false, cites: [{9.2}]`.
   * §5.1's *code nobody asked for* is that second question and not this one — it is counted by
   * {@link Counts.unclaimed}, which is the number a surface should show.
   */
  declares: boolean;
  /**
   * The sections this module names, **including the ones only its sibling test names**.
   *
   * A Rust module keeps its tests in the same file, so a `§` inside `#[cfg(test)]` has always
   * counted for free; folding in the `*.test.tsx` sibling is what stops one declaration answering
   * differently in the two languages. It is the symmetry, not a special case for TypeScript.
   *
   * **Not deduplicated by section, so `cites.length` is not a section count.** The núcleo dedups
   * by the whole citation on purpose: a file writing `§9.2 risk` in one place and `§9.2 spike` in
   * another yields two rows, because collapsing them would mean choosing which trailing candidate
   * survives and throwing the other away. Group by `.section` before counting or displaying.
   */
  cites: Citation[];
  tested: boolean;
}

export interface MapImport {
  from: string;
  to: string;
}

/**
 * A file that names a section in a language no reader here understands — the Go sidecars, mostly.
 *
 * **Not a {@link MapModule}, and never counted with one.** Nothing here knows what a Go file
 * imports or whether anything tests it, so calling one a module would let it borrow a confidence
 * this map never earned. It is carried anyway because without it the junction's *declared, with
 * no code* would be a lie: dozens of Go files under `sidecars/` name a `§`, and a decision one of
 * them implements would otherwise come back claimed by nothing at all.
 *
 * **This is the top-level `foreign`, and it is not {@link Anchored.foreign}.** That one is a
 * `string[]` of paths hanging off a single decision; this one is a whole file with its citations.
 * Same word, two levels, two shapes — which is why they have two names here.
 *
 * It has been on the wire since the structure layer shipped and this file never mirrored it.
 * Nothing broke and nothing announced it, because `apiFetch<T>` is a cast and a cast cannot
 * notice a field it was never told about.
 */
export interface ForeignFile {
  path: string;
  cites: Citation[];
}

/**
 * How firmly one approved decision is tied to code.
 *
 * **Four values, and no two of them may be merged.** Collapsing any pair would make a surface
 * claim something nobody measured — the false confidence this whole feature exists to cure,
 * reproduced inside the cure and wearing better pixels.
 *
 * - **`"declared"`** — a **readable** module names this section *and* names a document this
 *   project has, the shape §8 prescribes (`§6.4 workspace-de-projeto`). Certain, and the only
 *   value that may be drawn as a confirmation. **Zero of these exist in this repository today,
 *   and that is §8 unfixed rather than a defect in the join**: not one citation here says which
 *   document its `§` belongs to, so no positive join can be certain yet. A later slice proposes
 *   the edits that put a slug on them.
 * - **`"ambiguous"`** — something names the section and this map cannot confirm it meant *this*
 *   decision. **Two different uncertainties share the value.** One is *which document is this?* —
 *   a bare `§7` may be any spec's §7, and this repository has plenty of both. The other is *this
 *   is code I cannot read* — a Go sidecar names the section and nothing here knows what Go does
 *   with it. **The anchor does not tell them apart; {@link Anchored.modules} and
 *   {@link Anchored.foreign} do.** A surface drawing this value without looking at those two
 *   lists shows the weaker of the two claims for both. A fifth value was considered and refused,
 *   because the two are not disjoint: an unslugged citation inside a Go file is both at once, and
 *   a fifth word would have had to invent a precedence the spec never states.
 * - **`"silent"`** — **nothing anywhere** names this section: no module, no foreign file, nothing
 *   in any language scanned. This is §5.1's *declared, with no code*. **It is the one value here
 *   that is sound despite the ambiguity above** — if nothing names the section at all then
 *   nothing claims it under *any* document, whichever document each bare `§` meant. Decision 3
 *   says the value is in the nodes that do not match, which makes this the answer the map is
 *   certain about and the one it exists to give.
 * - **`"unnumbered"`** — the heading the decision was copied from carries no number, so nothing
 *   could be looked for. **Not a claim about the code.** A decision extracted from `## Contrato`
 *   is approved and real; it simply anchors nothing. Folding it into `"silent"` is the tempting
 *   move and the wrong one — *nothing claims this* is the report of a search, and here no search
 *   ran.
 */
export type Anchor = "declared" | "ambiguous" | "silent" | "unnumbered";

/** One approved decision, with everything the structure layer can say about it and nothing more. */
export interface Anchored {
  decision_id: number;
  /**
   * The line's number within its spec's extraction, carried through untouched.
   *
   * The owner approved that spec as a numbered list, and *line 3 of that document* is how they
   * refer to a decision afterwards — so a client can reproduce that order or choose another.
   */
  ordinal: number;
  spec_slug: string;
  /**
   * The heading as the model copied it — `## 4.1 Três tipos de decisão` — and not the number read
   * off it. The owner approved this string, so this is the string that goes back to them; the
   * number is an internal step and shows up only as {@link Anchored.anchor}.
   */
  section: string;
  text: string;
  /** The same two words {@link MapDecision.kind} carries, for the reason given on {@link DecisionKind}. */
  kind: DecisionKind;
  anchor: Anchor;
  /**
   * Readable modules naming this section, sorted by path. Empty for `"silent"` and
   * `"unnumbered"` — in the first case because nothing was found, in the second because nothing
   * was sought, and those are not the same emptiness.
   */
  modules: string[];
  /**
   * Paths naming it in a language this map cannot read.
   *
   * **A `string[]` of paths, and not the top-level {@link ForeignFile}[]** — the two `foreign`
   * keys on this payload sit at different levels and carry different shapes.
   *
   * **Separate from {@link Anchored.modules} and never merged into it.** *We know something is
   * there* and *we can see what it is* are different facts, and one list would let the second
   * borrow the first's confidence. It is also what keeps `"silent"` honest: reading *declared,
   * with no code* off the module list alone would report a whole language as absent.
   */
  foreign: string[];
}

/**
 * The numbers behind §5.3's header.
 *
 * They reconcile, and the núcleo asserts it rather than promising it:
 * `declared + ambiguous + silent + unnumbered === decisions`, and every module counted is in
 * exactly one of `unclaimed`, `unmatched`, or some {@link Anchored.modules}.
 *
 * **Not a percentage, and never one.** §12 refuses coverage metrics as a percentage outright — a
 * single collapsed number is exactly the collapse §5 forbids, and dividing any pair of these
 * would manufacture one.
 */
export interface Counts {
  decisions: number;
  declared: number;
  ambiguous: number;
  silent: number;
  unnumbered: number;
  unclaimed: number;
  unmatched: number;
}

/**
 * The intention layer read against the structure layer: what does not match.
 *
 * Two of the three lists are §5.1's rows and the third is the day-one reality. `decisions` holds
 * *declared, with no code* under the `"silent"` anchor; `unclaimed` is *code nobody asked for*;
 * `unmatched` is neither, and on day one is nearly every module because approval has barely
 * started (§10). Folding `unmatched` into `unclaimed` would inflate the one number §5.1 exists to
 * put in front of somebody.
 *
 * **Two of §5.1's four derived states are absent here, and the absence is not an omission.**
 * *À espera* and *silenciado* both mean the triager looked, and there is no triager yet (§6). A
 * surface that drew four states off this payload would be inventing two of them.
 */
export interface Junction {
  /** Ordered by `spec_slug`, then `ordinal`, then id — deterministic rather than final. */
  decisions: Anchored[];
  /** Modules naming no section at all. §5.1's *code nobody asked for*. */
  unclaimed: string[];
  /** Modules naming a section no approved decision names. Neither orphan nor matched. */
  unmatched: string[];
  counts: Counts;
}

export interface ProjectMap {
  modules: MapModule[];
  imports: MapImport[];
  /**
   * Files no reader here understands, kept rather than dropped.
   *
   * Mostly real code — the Go sidecars, the SQL migrations, the stylesheets and the scripts —
   * and keeping them is the whole point: a map that showed only what it could read would
   * quietly claim `sidecars/` does not exist. A tail of it was never source at all, being
   * assets and configuration. It measures what this map cannot see, and nothing that is wrong.
   */
  unread: string[];
  /** The subset of {@link ProjectMap.unread} that names a `§` at all, with the sections it names. */
  foreign: ForeignFile[];
  junction: Junction;
}

/**
 * Read when the mode opens, and not polled.
 *
 * The structure of a repository does not move between two ticks of a three-second timer, and
 * putting it on one would mean walking the whole tree off disk on every tick.
 */
export function useProjectMap(projectId: string | null) {
  return useQuery({
    queryKey: keys.projects.map(projectId ?? ""),
    queryFn: () => apiFetch<ProjectMap>(`/projects/${encodeURIComponent(projectId ?? "")}/map`),
    enabled: projectId !== null && projectId !== "",
  });
}

/**
 * The intention layer: what a model read out of a spec, waiting for its owner to say yes or no.
 *
 * `kind` really is spelled `countable`/`character` on the wire. The núcleo's own column holds
 * `b`/`c` under a `CHECK`, and translates on the way out on purpose — a list the owner is meant to
 * approve at a glance cannot label its two kinds with letters.
 */

export type DecisionKind = "countable" | "character";

/** Which model reads a spec. See {@link useExtractSpec} for why this is a call-time choice. */
export type Brain = "cloud" | "local";

/** One sentence a model read out of a spec. Field for field off `crate::map_store::Decision`. */
export interface MapDecision {
  id: number;
  spec_slug: string;
  section: string;
  ordinal: number;
  text: string;
  kind: DecisionKind;
  brain: string;
  extracted_at: string;
  /** `null` until the owner answers this line, one way or the other. */
  approved_at: string | null;
}

/**
 * Which specs this project has, named the way the owner reads them.
 *
 * Read once when the mode opens, and not polled — the same posture {@link useProjectMap} takes,
 * for the same reason: the document set does not move between two ticks of a three-second timer.
 */
export function useProjectSpecs(projectId: string | null) {
  return useQuery({
    queryKey: keys.projects.mapSpecs(projectId ?? ""),
    queryFn: () =>
      apiFetch<string[]>(`/projects/${encodeURIComponent(projectId ?? "")}/map/specs`),
    enabled: projectId !== null && projectId !== "",
  });
}

/**
 * The pile nobody has read yet.
 *
 * Not folded into {@link useProjectMap}: the structure layer is derived off disk on every read and
 * the pile is a table somebody writes to. One query invalidated by an approval would re-walk the
 * whole tree to answer a question the tree cannot answer.
 */
export function useMapDecisions(projectId: string | null) {
  return useQuery({
    queryKey: keys.projects.mapDecisions(projectId ?? ""),
    queryFn: () =>
      apiFetch<MapDecision[]>(`/projects/${encodeURIComponent(projectId ?? "")}/map/decisions`),
    enabled: projectId !== null && projectId !== "",
  });
}

/** What {@link useExtractSpec} sends: the document, and which model should read it. */
export interface ExtractSpecInput {
  specSlug: string;
  brain: Brain;
}

/**
 * Ask a model what one spec decided, and get back the whole pile — this extraction's rows landed
 * among whatever was already waiting, which is what the owner reads.
 *
 * `brain` is a parameter here and not a setting anywhere, because the choice is the owner's to make
 * per request and the núcleo will not make it for them: asked for `local` on a machine with none
 * configured, it refuses rather than quietly falling back to the cloud — a substitution nobody
 * would see coming, and one that spends money the owner did not agree to. A setting would turn that
 * refusal into somebody's permanent default; asking each time keeps it a decision.
 */
export function useExtractSpec(projectId: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ specSlug, brain }: ExtractSpecInput) =>
      apiFetch<MapDecision[]>(`/projects/${encodeURIComponent(projectId)}/map/extract`, {
        method: "POST",
        body: JSON.stringify({ spec_slug: specSlug, brain }),
      }),
    retry: false,
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: keys.projects.mapDecisions(projectId) });
    },
  });
}

/** What {@link useDecideMapLine} sends: the row, and the owner's answer to it. */
export interface DecideMapLineInput {
  id: number;
  approved: boolean;
}

/**
 * The owner's answer to one line.
 *
 * **204, so there is no body** (`post_project_map_decision`) — `void` is the honest type, and
 * `apiFetch` exempts 204 from its JSON parse, the way {@link useSetShadowVerdict} in `autopilot.ts`
 * already does for its own bodyless verdict. `404` means the row was already answered, belonged to
 * another project, or never existed — three reasons the daemon deliberately collapses into one,
 * because all three mean the same thing to whoever asked: *that line is not yours to answer now*.
 *
 * **Two keys are invalidated, and the map one is not optional.** An approval moves a line out of
 * the pile *and* into the junction — `map_store::approved` is exactly what `GET /projects/{id}/map`
 * joins against — so a map left holding its pre-approval counts is the feature lying about itself:
 * the owner answers a line, watches the pile shrink, and reads a junction that never heard about
 * it. Re-walking the tree is the price, and it is worth paying at the one moment somebody changed
 * the thing the walk is joined against.
 *
 * The two calls are written out even though `keys.projects.map` is a **prefix** of
 * `keys.projects.mapDecisions` and invalidating it alone would already reach the pile. Leaning on
 * that would make the pile's refresh a side effect of two key shapes happening to nest, and the
 * day somebody moves one of them the pile stops refreshing with nothing to say why. Two lines
 * state two intentions. {@link useExtractSpec} deliberately invalidates only the pile: extraction
 * produces rows nobody has approved, and nothing unapproved is in the junction to change.
 */
export function useDecideMapLine(projectId: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ id, approved }: DecideMapLineInput) =>
      apiFetch<void>(`/projects/${encodeURIComponent(projectId)}/map/decisions/${id}`, {
        method: "POST",
        body: JSON.stringify({ approved }),
      }),
    retry: false,
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: keys.projects.mapDecisions(projectId) });
      void queryClient.invalidateQueries({ queryKey: keys.projects.map(projectId) });
    },
  });
}
