import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { apiFetch } from "./client";
import { keys } from "./keys";

/**
 * The three lists a project declares about itself: what its worktrees may run without asking, what
 * the GitHub manager may do on its remote, and where a landing may be sent.
 *
 * **Not `project-commands.ts`.** That one holds commands somebody presses a button to run — `gate`,
 * `fmt`, `typecheck`. Nothing here is ever executed. These are permissions, and the two modules
 * share a first word and nothing else. `core/src/project_policy.rs` makes the same distinction in
 * the same words, and this is the shell's half of it.
 *
 * Nine routes, read off `core/src/http.rs` rather than inferred from the names:
 *
 * | what                          | route                                     |
 * |-------------------------------|-------------------------------------------|
 * | the two shell lists           | `GET /projects/{id}/shell-rules`          |
 * | declare or re-verdict one     | `POST /projects/{id}/shell-rules`         |
 * | withdraw one                  | `DELETE /projects/{id}/shell-rules`       |
 * | the autonomous GitHub ops     | `GET /projects/{id}/github-ops`           |
 * | grant one                     | `POST /projects/{id}/github-ops`          |
 * | withdraw one                  | `DELETE /projects/{id}/github-ops`        |
 * | the extra landing targets     | `GET /projects/{id}/land-targets`         |
 * | open one                      | `POST /projects/{id}/land-targets`        |
 * | close one                     | `DELETE /projects/{id}/land-targets`      |
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

/** One project's shell list, already split by verdict — `ShellRulesView` in `core/src/http.rs`. */
export interface ShellRules {
  allow: string[];
  deny: string[];
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
 * **What it still cannot do.** `GET /projects/{id}/shell-rules` serves two lists of prefixes and no
 * notes — the note is write-only through HTTP today — so nothing here can fetch the existing note
 * to send back for you. Preserving one means the page must hold the note it is editing in its own
 * form state. Until the GET carries notes, that half is a discipline and not a guarantee.
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
 * **The prefixes come back FOLDED, and that is the point of showing them at all.** The núcleo folds
 * a prefix through `classifier::normalize_command` on the way in and again on the way out, so a
 * rule typed `Remove-Item  -Recurse` is stored and enforced as `remove-item -recurse`. A surface
 * that echoed what somebody typed would be showing them a rule the classifier has never heard of.
 */
export function useProjectShellRules(projectId: string | null) {
  return useQuery({
    queryKey: keys.projects.shellRules(projectId ?? ""),
    queryFn: () =>
      apiFetch<ShellRules>(`/projects/${encodeURIComponent(projectId ?? "")}/shell-rules`),
    enabled: projectId !== null,
  });
}

/**
 * What the GitHub manager may do on this project's remote without asking.
 *
 * Operation names — `run_list`, `pr_view` — and not `gh` command lines, because a name is what can
 * tell `run_status` from `run_logs`. There is no route that lists the declarable ones: the set is
 * derived in `github.rs` from two compiled ceilings, and the only place it appears on the wire is
 * inside the `undeclarable_op` refusal, which names what would have been accepted. A page offering
 * a picker has to get its options from somewhere, and today that somewhere is a failed POST.
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
 * When flipping a verdict, pass `{ write: theNoteYouAreShowing }`.
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
 * lists the ops the ceilings admit, which is the only place that set reaches the shell.
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
 * PURE: which side a typed prefix is already declared on, or `null` for one that is not.
 *
 * Folds before it looks, because case is not part of a rule's identity — asking with the typed
 * spelling is how a form would offer to "create" a rule that already exists and then silently
 * overwrite it.
 *
 * `deny` is checked first. The unique index means a prefix can only be on one side, so the order
 * decides nothing today; it is the safe reading of a table that has somehow ended up disagreeing
 * with itself, and the same direction `shell_rules` takes about a verdict it cannot parse.
 */
export function declaredVerdict(rules: ShellRules, prefix: string): Verdict | null {
  const folded = foldPrefix(prefix);
  if (rules.deny.includes(folded)) return "deny";
  if (rules.allow.includes(folded)) return "allow";
  return null;
}
