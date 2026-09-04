import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { apiFetch } from "./client";
import { keys } from "./keys";

/**
 * The three lists a project declares about itself: what its worktrees may run without asking, what
 * the GitHub manager may do on its remote, and where a landing may be sent. And, because a form
 * offering a choice has to know what the choices are, the machine-wide catalogue of what MAY be
 * declared of the second one.
 *
 * **Not `project-commands.ts`.** That one holds commands somebody presses a button to run — `gate`,
 * `fmt`, `typecheck`. Nothing here is ever executed. These are permissions, and the two modules
 * share a first word and nothing else. `core/src/project_policy.rs` makes the same distinction in
 * the same words, and this is the shell's half of it.
 *
 * Ten routes, read off `core/src/http.rs` rather than inferred from the names:
 *
 * | what                          | route                                     |
 * |-------------------------------|-------------------------------------------|
 * | this project's shell rules    | `GET /projects/{id}/shell-rules`          |
 * | declare or re-verdict one     | `POST /projects/{id}/shell-rules`         |
 * | withdraw one                  | `DELETE /projects/{id}/shell-rules`       |
 * | the autonomous GitHub ops     | `GET /projects/{id}/github-ops`           |
 * | grant one                     | `POST /projects/{id}/github-ops`          |
 * | withdraw one                  | `DELETE /projects/{id}/github-ops`        |
 * | the extra landing targets     | `GET /projects/{id}/land-targets`         |
 * | open one                      | `POST /projects/{id}/land-targets`        |
 * | close one                     | `DELETE /projects/{id}/land-targets`      |
 * | what MAY be declared          | `GET /github/declarable-ops`              |
 *
 * **The tenth hangs off no project, and that is a fact about the answer rather than about the URL.**
 * The declarable set is compiled into the daemon — two ceilings intersected with the operations it
 * can build — so it is the same for every project on the roster and changes only when the daemon is
 * rebuilt. A route under `/projects/{id}/…` would have taken an id that changed nothing.
 *
 * **The three DELETEs carry what they delete in the BODY.** A shell prefix contains spaces, slashes
 * and dots and is not a safe path segment; the other two follow it rather than splitting one shape
 * three ways. That is unusual enough that every `forget` hook below says so again at its own door.
 *
 * **Not polled.** A declaration list changes when a person edits it and at no other time — the same
 * reading `useProjectCommands` takes about its own rows, minus the running command that makes it
 * poll at all. What a write here changes *immediately* is not this list but the núcleo's next
 * decision: `hooks.rs` reads the shell table per tool call, with no cache and no restart, so a rule
 * declared here binds the very next tool call of every in-flight run of this project. The list on
 * screen is a picture of the table; the table is already in force.
 */

/* ----------------------------------------------------------------- shapes -- */

/**
 * What a shell rule says about the prefix it names — `project_policy::Verdict`, lower-cased on the
 * wire by its `rename_all`.
 *
 * The two are not mirror images, and a page that draws them as one control with two ends will
 * mislead. An `allow` is refused by the núcleo unless `classifier::shell_form_is_readable` accepts
 * the prefix, because `classify_segment` guards `rules.allows` with that same check and a
 * permission of the wrong shape would be stored and never fire. A `deny` is refused for nothing:
 * `rules.denies` answers at the line level with no shape guard over it, so `tail -f`, `sort -o`,
 * `find . -exec` and `curl … | sh` can all be refused — and those are the refusals most worth
 * writing down.
 */
export type Verdict = "allow" | "deny";

/**
 * One declared shell rule, whole — `ShellRuleView` in `core/src/http.rs`.
 *
 * **A row and not two lists of prefixes, and the difference is the note.** The route used to serve
 * `{ allow: string[], deny: string[] }`, which could carry no note at all — so the column migration
 * `0128` calls "a única defesa contra uma lista que daqui a seis meses ninguém sabe justificar" was
 * write-only, and {@link Note}'s trap had no exit: an editor flipping a verdict has to resend the
 * existing justification and had no way to fetch it.
 *
 * The verdict rides on the rule for the same reason. In the old shape it lived in the CONTAINER, so
 * the moment a page handed one rule to a form the verdict fell off — and the POST rewrites both
 * fields from what it is sent. What comes back here is field-for-field what {@link useDeclareShellRule}
 * takes, so re-declaring is changing one field of a row you are already holding.
 *
 * The allow/deny split is not lost by this: it is on every row instead of being named once at the
 * top, and grouping by `verdict` is a filter. `deny` beats `allow` in the classifier — see
 * {@link Verdict} — which is now a sentence a page can put beside a rule rather than beside a heading.
 */
export interface ShellRule {
  /** FOLDED, as stored and as enforced — see {@link foldPrefix}. */
  prefix: string;
  verdict: Verdict;
  /** `null` for a rule nobody justified. An absent justification, never an absent field. */
  note: string | null;
  /**
   * When this prefix was FIRST declared, in the daemon's `datetime('now')` spelling —
   * `2026-09-04 13:07:18`, UTC, space-separated and NOT RFC 3339. `new Date(…)` will not parse it
   * portably; treat it as the daemon's text unless you convert it deliberately.
   *
   * **Not "last changed".** `declare_shell_rule`'s `ON CONFLICT DO UPDATE` sets `verdict` and `note`
   * and deliberately leaves this column alone, so a rule re-verdicted this morning still carries the
   * day somebody wrote it down. A caption saying "edited" would be inventing a fact the daemon does
   * not hold.
   */
  created_at: string;
}

/**
 * Which door a GitHub operation goes through, and it is not cosmetic.
 *
 * A declared `read` binds the very next decision: `Policy::for_project` splits a project's stored
 * rows on the reading half, and that half is already consulted through the Bash door. A declared
 * `action` is recorded and INERT — the route answers 204 just the same, and a later step wires it.
 * A page that drew the two identically would be promising an owner their declared action is in
 * force. See {@link useDeclareGithubOp}, which says the same thing at the write.
 */
export type OpHalf = "read" | "action";

/**
 * One GitHub operation this daemon can build — `DeclarableOpView` in `core/src/http.rs`.
 *
 * A row per operation for {@link ShellRule}'s reason, and the two landed together: a row that
 * travels alone keeps its facts, where a name lifted out of an "admitted" list into a form has
 * already forgotten which list it came from.
 */
export interface DeclarableOp {
  /** The typed name, as `op_kind` goes over the wire — `run_list`, `pr_comment`. */
  kind: string;
  half: OpHalf;
  /**
   * Whether a project may declare it. `false` is a fact about the build and not a state anything on
   * screen can change: the compiled ceilings do not admit it, so no route, no file and no owner can
   * turn it on.
   */
  declarable: boolean;
}

/**
 * What a declaration does to the rule's note, spelled as a choice the caller has to make.
 *
 * **This union exists to close a trap, and the trap is worth stating in full.**
 * `declare_shell_rule`'s `ON CONFLICT … DO UPDATE SET verdict = excluded.verdict, note =
 * excluded.note` is a plain overwrite and deliberately not a `COALESCE` — the route says "this rule
 * is now in this state", and a note that fell back to the stored one could never be removed. The
 * consequence is that **a second POST of the same prefix carrying no `note` writes `NULL` over the
 * one that was there**: an editor that sends only `prefix` and `verdict` to flip a verdict gets a
 * 204 and silently discards the justification. Migration `0128` calls that column "a única defesa
 * contra uma lista que daqui a seis meses ninguém sabe justificar".
 *
 * An optional `note?: string` would have made that trap the *default*: leave the field out, lose
 * the note, learn nothing. So the field is not optional and not `string | null` either — `null` is
 * too easy to read as "unchanged", which is the one thing the route cannot do. There are two
 * operations and this type has two members, so the compiler asks which one you meant.
 *
 * **And the note to resend is now fetchable.** {@link ShellRule} carries it, so preserving a
 * justification through a verdict change is reading it off the row you are already showing — see
 * {@link declaredRule} — and sending it back as `{ write: … }`. It was a discipline while the GET
 * served prefixes only; it is an operation now. The type still asks, because a row whose `note` is
 * `null` maps onto `{ erase: true }` and nothing else, and erasing is a thing somebody may mean.
 */
export type Note =
  /** Store this justification, replacing whatever the rule carried before. */
  | { write: string }
  /** Deliberately leave the rule with no justification. This ERASES an existing one. */
  | { erase: true };

/** One shell rule, as declared. The prefix is folded before it is stored — see {@link foldPrefix}. */
export interface ShellRuleDeclaration {
  projectId: string;
  prefix: string;
  verdict: Verdict;
  note: Note;
}

/**
 * `note` on the wire: the string, or an explicit `null`.
 *
 * Built by hand rather than spread out of the input, because all four request bodies carry
 * `#[serde(deny_unknown_fields)]` — a stray `projectId` riding along in a spread would be a 422 and
 * not a field the daemon politely ignores.
 */
function noteField(note: Note): string | null {
  return "write" in note ? note.write : null;
}

/* ------------------------------------------------------------------ reads -- */

/**
 * What this project's worktrees may run without asking, and what they may never run.
 *
 * One row per rule, ordered by prefix, each carrying its own verdict — {@link ShellRule} argues why
 * that rather than two lists of prefixes. Grouping them for the screen is a filter over `verdict`.
 *
 * **The prefixes come back FOLDED, and that is the point of showing them at all.** The núcleo folds
 * a prefix through `classifier::normalize_command` on the way in and again on the way out, so a
 * rule typed `Remove-Item  -Recurse` is stored and enforced as `remove-item -recurse`. A surface
 * that echoed what somebody typed would be showing them a rule the classifier has never heard of.
 */
export function useProjectShellRules(projectId: string | null) {
  return useQuery({
    queryKey: keys.projects.shellRules(projectId ?? ""),
    queryFn: () =>
      apiFetch<ShellRule[]>(`/projects/${encodeURIComponent(projectId ?? "")}/shell-rules`),
    enabled: projectId !== null,
  });
}

/**
 * What the GitHub manager may do on this project's remote without asking.
 *
 * Operation names — `run_list`, `pr_view` — and not `gh` command lines, because a name is what can
 * tell `run_status` from `run_logs`. What this project HAS declared; what it MAY declare is
 * {@link useDeclarableGithubOps}, and a picker needs both — this one to know which boxes are
 * ticked, that one to know which boxes exist.
 */
export function useProjectGithubOps(projectId: string | null) {
  return useQuery({
    queryKey: keys.projects.githubOps(projectId ?? ""),
    queryFn: () =>
      apiFetch<string[]>(`/projects/${encodeURIComponent(projectId ?? "")}/github-ops`),
    enabled: projectId !== null,
  });
}

/**
 * Every GitHub operation this daemon can build, and whether a project may declare it —
 * `GET /github/declarable-ops`.
 *
 * **The set used to reach the wire only inside a refusal.** `POST /projects/{id}/github-ops`
 * validates against it, and until this route the only place it appeared was `undeclarable_op`'s
 * `detail`. A picker had two options and both were wrong: hardcode the names, which drifts from the
 * compiled ceilings in silence and in the direction that offers something the daemon refuses, or
 * discover the list by POSTing something invalid.
 *
 * **Every operation, with a flag, and the `false` ones are the reason.** An operation outside the
 * ceilings — `api_read` is the standing example, and not even `.ai/github.yaml` can turn it on — is
 * a FACT to show and never a control to draw. Serving only the admitted names would leave a page
 * two bad choices again: omit it, which claims this daemon cannot do it at all, or draw a checkbox
 * that cannot be ticked, which is a lie about who decides. `declarable: false` is how it gets drawn
 * as what it is.
 *
 * **Machine-wide, so it takes no project id and is keyed apart from the three lists.** A declaration
 * write invalidates `keys.projects.all` and must not throw this away — it cannot have changed, and
 * it cannot change while the daemon is running.
 *
 * Not polled, and this one is not even "changes when a person edits it": it changes when the daemon
 * is rebuilt, which the app finds out about by being restarted alongside it.
 */
export function useDeclarableGithubOps() {
  return useQuery({
    queryKey: keys.github.declarableOps,
    queryFn: () => apiFetch<DeclarableOp[]>("/github/declarable-ops"),
  });
}

/**
 * Where a `--land` may be sent in this project, besides its integration branch.
 *
 * The integration branch is admissible with no row, so it is never in this list — an empty list
 * means "nowhere but the usual place", not "nowhere".
 */
export function useProjectLandTargets(projectId: string | null) {
  return useQuery({
    queryKey: keys.projects.landTargets(projectId ?? ""),
    queryFn: () =>
      apiFetch<string[]>(`/projects/${encodeURIComponent(projectId ?? "")}/land-targets`),
    enabled: projectId !== null,
  });
}

/* ----------------------------------------------------------------- writes -- */

/**
 * Every declaration write settles the same way, so it is written once.
 *
 * `keys.projects.all`, as `project-commands.ts` invalidates it: the three lists live under that
 * prefix, and one form can move more than one of them. `onSettled` rather than `onSuccess` because
 * a 404 or a 423 means the picture on screen is wrong either way.
 *
 * `retry: false`, and here it is not merely the house default restated. Every refusal these nine
 * routes give is settled — the stop is engaged, the project is not on the roster, the prefix could
 * never fire, the rule was never declared — and asking again gets the same sentence one second
 * later. The one thing a retry would change is the record: a POST that half-landed and was sent
 * twice is a second write to a table that governs what an autonomous run may do.
 */
function useDeclarationWrite<Input>(mutationFn: (input: Input) => Promise<void>) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn,
    retry: false,
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: keys.projects.all });
    },
  });
}

/**
 * Declare a shell rule, or change the verdict of one already declared.
 *
 * One hook for both, because the núcleo has one operation: the identity of a rule is its folded
 * prefix, and a second POST of the same prefix is an EDIT rather than a second row. A separate
 * `useEditShellRule` would be the same request wearing a name that suggests the daemon can tell the
 * difference. It cannot, and {@link Note} is where that costs the caller something.
 *
 * **Sending a note back is how you keep one.** See {@link Note}: this route rewrites the note on
 * every declaration, so `{ erase: true }` on a rule that carried a justification throws it away.
 * When flipping a verdict, read the note off the row {@link useProjectShellRules} already handed
 * you — {@link declaredRule} finds it — and send it back as `{ write: … }`. A row whose `note` is
 * `null` had none to keep, and `{ erase: true }` is the honest way to say so.
 *
 * Refuses with `kill_switch` (423) for an `allow` while the emergency stop is engaged, and never
 * for a `deny` — a stop that stopped somebody narrowing autonomy would be holding the door open on
 * the way out. `unmatchable_prefix` (422) names the prefix and says it would be enforced as a
 * `deny`; `no_such_project` (404) is a project this daemon has never heard of. Every one of those
 * carries a `detail` worth putting on screen verbatim.
 */
export function useDeclareShellRule() {
  return useDeclarationWrite(({ projectId, prefix, verdict, note }: ShellRuleDeclaration) =>
    apiFetch<void>(`/projects/${encodeURIComponent(projectId)}/shell-rules`, {
      method: "POST",
      body: JSON.stringify({ prefix, verdict, note: noteField(note) }),
    }),
  );
}

/**
 * Withdraw one shell rule.
 *
 * **The prefix goes in the body.** Not a quirk to tidy away: a prefix carries spaces and slashes,
 * and encoding one into a path segment only to decode it again buys nothing. `no_such_rule` (404)
 * when nothing matched, and its `detail` names the prefix in its FOLDED spelling — which is the
 * answer a caller who typed the wrong case needs to see.
 */
export function useForgetShellRule() {
  return useDeclarationWrite(({ projectId, prefix }: { projectId: string; prefix: string }) =>
    apiFetch<void>(`/projects/${encodeURIComponent(projectId)}/shell-rules`, {
      method: "DELETE",
      body: JSON.stringify({ prefix }),
    }),
  );
}

/**
 * Grant one GitHub operation to this project without asking.
 *
 * Idempotent: a second declaration finds a row already saying what it came to say and leaves it,
 * `created_at` and all.
 *
 * **A declared READ is live; a declared ACTION is recorded and inert.** The reading half is already
 * consulted through the Bash door, so granting `run_list` changes the next decision. The acting
 * half — `pr_comment` and its neighbours — is stored and awaits a later step, and the route
 * answers 204 for it just the same. A page that promised an owner their declared action was in
 * force would be describing a step that has not landed.
 *
 * Gated by the emergency stop, like an `allow`: this widens, immediately. `undeclarable_op` (422)
 * lists the ops the ceilings admit — which is worth putting on screen verbatim, but is no longer
 * the only way to learn the set: {@link useDeclarableGithubOps} serves it, so a picker need never
 * offer something this route will refuse.
 */
export function useDeclareGithubOp() {
  return useDeclarationWrite(({ projectId, opKind }: { projectId: string; opKind: string }) =>
    apiFetch<void>(`/projects/${encodeURIComponent(projectId)}/github-ops`, {
      method: "POST",
      body: JSON.stringify({ op_kind: opKind }),
    }),
  );
}

/** Withdraw one GitHub operation. In the body, like its siblings; `no_such_op` (404) when it was never declared. */
export function useForgetGithubOp() {
  return useDeclarationWrite(({ projectId, opKind }: { projectId: string; opKind: string }) =>
    apiFetch<void>(`/projects/${encodeURIComponent(projectId)}/github-ops`, {
      method: "DELETE",
      body: JSON.stringify({ op_kind: opKind }),
    }),
  );
}

/**
 * Open one more landing target for this project.
 *
 * **A branch that does not exist yet is accepted, and a page must not "help" by checking first.**
 * The branch is often made by the very run that will land into it; existence is
 * `land::resolve_target`'s question, asked at the moment of landing. What IS checked here is the
 * NAME — `vcs::Branch::new`, which refuses an empty name, a leading `-`, and whitespace or control
 * characters — so `unusable_branch` (422) is about the spelling and never about the branch being
 * missing. Not gated by the stop: a target binds nothing until a landing is attempted, and a
 * landing goes through the queue, which the stop already governs.
 */
export function useDeclareLandTarget() {
  return useDeclarationWrite(({ projectId, branch }: { projectId: string; branch: string }) =>
    apiFetch<void>(`/projects/${encodeURIComponent(projectId)}/land-targets`, {
      method: "POST",
      body: JSON.stringify({ branch }),
    }),
  );
}

/**
 * Close one landing target.
 *
 * `no_such_target` (404) for a branch that was never opened — including the integration branch,
 * which needs no row to stay admissible. That 404 is the honest answer and not a failure to
 * understand: there was no target of that name to close.
 */
export function useForgetLandTarget() {
  return useDeclarationWrite(({ projectId, branch }: { projectId: string; branch: string }) =>
    apiFetch<void>(`/projects/${encodeURIComponent(projectId)}/land-targets`, {
      method: "DELETE",
      body: JSON.stringify({ branch }),
    }),
  );
}

/* ------------------------------------------------------------ the spelling -- */

/**
 * PURE: what the núcleo will actually store for a typed prefix.
 *
 * A mirror of `classifier::normalize_command` — collapse whitespace, lower-case — and the shell's
 * only reason to have one is that the fold happens on the way IN. Between somebody typing
 * `Remove-Item  -Recurse` and the list coming back saying `remove-item -recurse`, a form that could
 * not say what would be stored is a form that surprises people, and a page that could not fold
 * cannot tell a NEW rule from an edit of one that exists — which is precisely the moment
 * {@link Note} matters.
 *
 * ASCII-only lower-casing, because `to_ascii_lowercase` is what the núcleo does; a plain
 * `toLowerCase()` would fold `İ` where the núcleo leaves it alone and quietly disagree about the
 * identity of a rule. The residue is Rust's Unicode whitespace set against JavaScript's `\s`, which
 * differ over a handful of exotic separators no command line has ever carried. This is a preview
 * and never the decision: the list that comes back from the daemon is the folded truth.
 */
export function foldPrefix(prefix: string): string {
  return prefix
    .split(/\s+/)
    .filter((token) => token !== "")
    .join(" ")
    .replace(/[A-Z]/g, (letter) => letter.toLowerCase());
}

/**
 * PURE: the rule already declared for a typed prefix, or `null` for one that is not.
 *
 * Folds before it looks, because case is not part of a rule's identity — asking with the typed
 * spelling is how a form would offer to "create" a rule that already exists and then silently
 * overwrite it.
 *
 * **The whole row and not just the verdict, because the row is what an edit has to send back.**
 * `POST /projects/{id}/shell-rules` rewrites `verdict` and `note` from what it is given, so
 * preserving a justification through a verdict change is reading `note` off what this returns and
 * sending it as {@link Note}. The row's `note` is `string | null` and `Note` has no `null`, which is
 * the type asking the one question that matters: a rule that carried no justification is
 * `{ erase: true }`, deliberately, and never an empty `write`. See {@link Note} for the trap.
 *
 * A `deny` is preferred over an `allow` if the list somehow carries both. The unique index means a
 * prefix can only be on one side, so the choice decides nothing today; it is the safe reading of a
 * table that has ended up disagreeing with itself, and the same direction `shell_rules` takes about
 * a verdict it cannot parse.
 */
export function declaredRule(rules: ShellRule[], prefix: string): ShellRule | null {
  const folded = foldPrefix(prefix);
  const matching = rules.filter((rule) => rule.prefix === folded);
  return matching.find((rule) => rule.verdict === "deny") ?? matching[0] ?? null;
}

/**
 * PURE: which side a typed prefix is already declared on, or `null` for one that is not.
 *
 * {@link declaredRule} with the other three fields dropped, for the one caller that genuinely only
 * asks which side — a form deciding whether it is creating a rule or editing one. Anything about to
 * WRITE wants the row, because the note is on it.
 */
export function declaredVerdict(rules: ShellRule[], prefix: string): Verdict | null {
  return declaredRule(rules, prefix)?.verdict ?? null;
}
