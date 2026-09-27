import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { apiFetch } from "./client";
import { keys } from "./keys";

/**
 * The four lists a project declares about itself: what its worktrees may run without asking, what
 * the GitHub manager may do on its remote, what the shared git queue may do on its repository, and
 * where a landing may be sent. And, because a form offering a choice has to know what the choices
 * are, the machine-wide catalogues of what MAY be declared of the second and third ones.
 *
 * **Not `project-commands.ts`.** That one holds commands somebody presses a button to run — `gate`,
 * `fmt`, `typecheck`. Nothing here is ever executed. These are permissions, and the two modules
 * share a first word and nothing else. `core/src/project_policy.rs` makes the same distinction in
 * the same words, and this is the shell's half of it.
 *
 * Fourteen routes, read off `core/src/http.rs` rather than inferred from the names:
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
 * | what MAY be declared remotely | `GET /github/declarable-ops`              |
 * | the autonomous git ops        | `GET /projects/{id}/git-ops`              |
 * | grant one                     | `POST /projects/{id}/git-ops`             |
 * | withdraw one                  | `DELETE /projects/{id}/git-ops`           |
 * | what MAY be declared locally  | `GET /vcs/declarable-ops`                 |
 *
 * **The tenth and fourteenth hang off no project, and that is a fact about the answer rather than
 * about the URL.** The declarable sets are compiled into the daemon — two ceilings intersected with
 * the operations it can build remotely, and every operation the shared git queue can build locally
 * — so they are the same for every project on the roster and change only when the daemon is rebuilt.
 * A route under `/projects/{id}/…` would have taken an id that changed nothing.
 *
 * **The four DELETEs carry what they delete in the BODY.** A shell prefix contains spaces, slashes
 * and dots and is not a safe path segment; the other three follow it rather than splitting one
 * shape four ways. That is unusual enough that every `forget` hook below says so again at its own
 * door.
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
  /** FOLDED, as stored and as enforced — see {@link foldPrefix} and {@link foldPathPrefix}. */
  prefix: string;
  /**
   * Which tool's writes this rule governs — `"Edit"` or `"Write"` — or `null` for a rule about a
   * COMMAND prefix.
   *
   * **The field that tells the two kinds of rule apart, and nothing else can.** `deny migrations`
   * is a command nobody may run here; `deny Edit migrations` is a directory nothing may write into.
   * They may both be declared at once — the núcleo's unique index is `(project_id, tool, prefix)` —
   * so a page keying a row on the prefix alone draws one row where there are two, and a page
   * showing the prefix alone shows a path as if it were a command.
   *
   * **A write rule can only ever say `deny`.** `POST /projects/{id}/shell-rules` refuses an `allow`
   * that names a tool with `unenforceable_allow`, because the write chain in `classifier::classify`
   * has no allow side to reach and `project_policy::shell_rules` drops such a row on the way out.
   * A control offering to flip one is a control that knows the request will be refused.
   *
   * It also says which FOLD the prefix went through: `null` was lower-cased by
   * {@link foldPrefix}, a tool name means {@link foldPathPrefix}, which keeps a path's case.
   */
  tool: string | null;
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
 * One git operation the shared queue can build — `DeclarableGitOpView` in `core/src/http.rs`.
 *
 * Unlike {@link DeclarableOp}, this has no `half`: every operation here is a write performed by the
 * queue, so the GitHub distinction between a live read and a recorded, inert action has no meaning.
 * `declarable` remains explicit because the catalogue is a fact about this daemon build.
 */
export interface DeclarableGitOp {
  /** The typed name, as `op_kind` goes over the wire — `push`, `branch-delete`. */
  kind: string;
  /** Whether a project may declare this operation on this daemon build. */
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
  /**
   * `"Edit"` or `"Write"` for a rule about writes to a PATH, `null` for one about a command prefix.
   *
   * Not optional, for {@link Note}'s reason in a smaller key: the two mean different things to the
   * daemon and the field is the only thing that says which was meant. A `null` written out is a
   * caller saying "a command", where an omitted field would be a caller who did not think about it
   * — and the route reads both the same way, so the compiler is the only place the difference can
   * still be asked about.
   *
   * `unknown_tool` (422) for anything else, and `unenforceable_allow` (422) for a tool beside an
   * `allow` — see {@link ShellRule.tool}.
   */
  tool: string | null;
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
 * Which git operations this project lets the shared queue perform for an autonomous run.
 *
 * A tick does not let the agent run the command in its shell: the tool call is still denied and
 * returns a ticket id, while the queue receives that ticket as already consented and performs the
 * operation. This raw list is what the project HAS declared; {@link useDeclarableGitOps} says what
 * this daemon can build, and a picker needs both so a stranded declaration remains withdrawable.
 */
export function useProjectGitOps(projectId: string | null) {
  return useQuery({
    queryKey: keys.projects.gitOps(projectId ?? ""),
    queryFn: () => apiFetch<string[]>(`/projects/${encodeURIComponent(projectId ?? "")}/git-ops`),
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
 * ceilings — `api_read` is the standing example, and not even `~/.nucleos/github.yaml` can turn it on — is
 * a FACT to show and never a control to draw. Serving only the admitted names would leave a page
 * two bad choices again: omit it, which claims this daemon cannot do it at all, or draw a checkbox
 * that cannot be ticked, which is a lie about who decides. `declarable: false` is how it gets drawn
 * as what it is.
 *
 * **Machine-wide, so it takes no project id and is keyed apart from the four lists.** A declaration
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
 * Every git operation this daemon's shared queue can build, and whether a project may declare it —
 * `GET /vcs/declarable-ops`.
 *
 * Machine-wide and compiled into the daemon, so it takes no project id and has a cache root apart
 * from project declarations. The project read stays raw on purpose: if a later build drops a kind,
 * its stored row must remain visible as a fact its owner can withdraw.
 */
export function useDeclarableGitOps() {
  return useQuery({
    queryKey: keys.vcs.declarableOps,
    queryFn: () => apiFetch<DeclarableGitOp[]>("/vcs/declarable-ops"),
  });
}

/**
 * Everywhere a `--land` may be sent in this project — `land::IntegrationBranch`, tagged on `state`.
 *
 * **Four arms because a branch name on screen looks equally true in all four**, and in three of them
 * it is not admissible. A page that rendered a name and captioned it "always admissible" would be
 * asserting something the núcleo refuses, with nothing on screen to say which case it was in.
 */
export type IntegrationBranch =
  /** The column declares it and the ref is there. Admissible, full stop. */
  | { state: "declared"; branch: string }
  /** Declared and the ref is gone, or there is no folder to confirm it in. Nothing lands by default. */
  | { state: "stale"; branch: string }
  /** Nothing declared; this is what the first landing would derive and write down. */
  | { state: "derived"; branch: string }
  /** No answer, with the daemon's own sentence for why. */
  | { state: "unknown"; why: string };

/**
 * Where this project's work lands: the default, and the alternatives it admits —
 * `GET /projects/{id}/land-targets`.
 *
 * The integration branch is admissible with no row, so it is never in `targets` — an empty list
 * means "nowhere but the usual place", not "nowhere".
 */
export interface Landing {
  integration: IntegrationBranch;
  targets: string[];
}

/**
 * Everywhere a `--land` may be sent in this project.
 *
 * **Both halves come from here, and the second half is why the shape changed.** The route used to
 * serve the extra targets alone and the page filled the default in from
 * `GET /projects/{id}/branches` — which is `inspect::Branches::integration`, the branch the main
 * checkout is *parked on*. Its own doc in the núcleo says that field is a heuristic kept from before
 * `land.rs` existed, and that the design creating `land.rs` killed that read *"precisely because a
 * checkout parked on the wrong branch silently redirected every landing"*. So a project whose clone
 * sat on a feature branch showed that branch, captioned admissible, while `--land` refused it by
 * name and the branch that is admissible appeared nowhere on the page.
 *
 * One route now answers both halves, because one module owns the question.
 */
export function useProjectLandTargets(projectId: string | null) {
  return useQuery({
    queryKey: keys.projects.landTargets(projectId ?? ""),
    queryFn: () =>
      apiFetch<Landing>(`/projects/${encodeURIComponent(projectId ?? "")}/land-targets`),
    enabled: projectId !== null,
  });
}

/* ----------------------------------------------------------------- writes -- */

/**
 * Every declaration write settles the same way, so it is written once.
 *
 * **The four lists by name, and NOT the `keys.projects.all` prefix they share.** It used to be the
 * prefix, on the argument that the four live under it and one form can move more than one of them —
 * which was true while they were the only things there. `keys.projects.githubRepo` is under it now,
 * and that read spawns `git remote get-url` against the project root; a prefix invalidation made
 * every tick of a checkbox re-run a subprocess for a fact no declaration can change. Naming the
 * four is also the honest statement of what these writes touch.
 *
 * `onSettled` rather than `onSuccess` because a 404 or a 423 means the picture on screen is wrong
 * either way.
 *
 * `retry: false`, and here it is not merely the house default restated. Every refusal these twelve
 * routes give is settled — the stop is engaged, the project is not on the roster, the prefix could
 * never fire, the rule was never declared — and asking again gets the same sentence one second
 * later. The one thing a retry would change is the record: a POST that half-landed and was sent
 * twice is a second write to a table that governs what an autonomous run may do.
 */
function useDeclarationWrite<Input extends { projectId: string }>(
  mutationFn: (input: Input) => Promise<void>,
) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn,
    retry: false,
    // The constraint is what makes this safe rather than a cast: every declaration write names the
    // project it is about, so the keys to refresh are computable from the input the caller already
    // had to supply.
    onSettled: (_data, _error, { projectId }) => {
      for (const key of [
        keys.projects.shellRules(projectId),
        keys.projects.githubOps(projectId),
        keys.projects.gitOps(projectId),
        keys.projects.landTargets(projectId),
      ]) {
        void queryClient.invalidateQueries({ queryKey: key });
      }
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
  return useDeclarationWrite(({ projectId, prefix, tool, verdict, note }: ShellRuleDeclaration) =>
    apiFetch<void>(`/projects/${encodeURIComponent(projectId)}/shell-rules`, {
      method: "POST",
      body: JSON.stringify({ prefix, tool, verdict, note: noteField(note) }),
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
 *
 * **And the tool goes with it, because a prefix alone no longer names a rule.** A project may hold
 * `deny migrations` and `deny Edit migrations` at the same time; the daemon's `WHERE` matches on
 * `(project_id, tool, prefix)`, so a DELETE that left the tool out would take the command rule
 * while the write rule stayed on screen. Sending it is how a caller says which of the two it meant,
 * and `null` says the command one — the reading every DELETE written before the field existed had.
 */
export function useForgetShellRule() {
  return useDeclarationWrite(
    ({ projectId, prefix, tool }: { projectId: string; prefix: string; tool: string | null }) =>
      apiFetch<void>(`/projects/${encodeURIComponent(projectId)}/shell-rules`, {
        method: "DELETE",
        body: JSON.stringify({ prefix, tool }),
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
 * Grant one git operation to the shared queue for this project's autonomous runs.
 *
 * Presence is the grant, and a second declaration is idempotent. The emergency stop refuses this
 * widening with 423; an unknown project is 404 and a kind outside the compiled catalogue is 422.
 * None of those refusals should be retried: each describes settled authority, not a transient read.
 */
export function useDeclareGitOp() {
  return useDeclarationWrite(({ projectId, opKind }: { projectId: string; opKind: string }) =>
    apiFetch<void>(`/projects/${encodeURIComponent(projectId)}/git-ops`, {
      method: "POST",
      body: JSON.stringify({ op_kind: opKind }),
    }),
  );
}

/**
 * Withdraw one git operation from this project's autonomous queue grants.
 *
 * The kind travels in the body like the other declaration families. Withdrawal narrows authority,
 * so the emergency stop does not gate it; 404 means there was no stored declaration to remove.
 */
export function useForgetGitOp() {
  return useDeclarationWrite(({ projectId, opKind }: { projectId: string; opKind: string }) =>
    apiFetch<void>(`/projects/${encodeURIComponent(projectId)}/git-ops`, {
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
 * PURE: what the núcleo will actually store for a typed PATH prefix, and deliberately not
 * {@link foldPrefix}.
 *
 * A mirror of `project_policy::fold_path_prefix`: trim, write `\` as `/` so a Windows spelling and
 * a POSIX one are one prefix, drop a trailing `/` so `migrations` and `migrations/` are one prefix
 * — and **no lower-casing**, which is the whole reason there are two of these.
 *
 * `foldPrefix` lower-cases because a command is case-insensitive to us: `Remove-Item` and
 * `remove-item` are one cmdlet whatever the filesystem thinks. A path's case is the FILESYSTEM's
 * business, and the núcleo answers that question at comparison time, in `write_denied_by_project`,
 * with the fold the filesystem itself uses. A preview that lower-cased would be showing an owner a
 * path that is not the one being stored — which is the lie the preview exists to prevent, told from
 * the other side.
 *
 * A preview and never the decision, like its sibling: what comes back from the daemon is the folded
 * truth.
 */
export function foldPathPrefix(prefix: string): string {
  return prefix.trim().replace(/\\/g, "/").replace(/\/+$/, "");
}

/**
 * PURE: the rule already declared for a typed prefix under a given tool, or `null` for one that is
 * not.
 *
 * Folds before it looks, because the typed spelling is not a rule's identity — asking with it is how
 * a form would offer to "create" a rule that already exists and then silently overwrite it. WHICH
 * fold follows the tool, exactly as it does in the núcleo: a command goes through
 * {@link foldPrefix} and loses its case, a path through {@link foldPathPrefix} and keeps it.
 *
 * **The tool is part of what is being asked, and defaults to `null` because that is the older
 * question.** `deny migrations` and `deny Edit migrations` are two rules the daemon lets a project
 * hold at once, so a lookup that ignored the tool would answer about the wrong one — telling a form
 * that the `Edit` rule it is about to declare already exists, and offering it the command rule's
 * justification to keep.
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
export function declaredRule(
  rules: ShellRule[],
  prefix: string,
  tool: string | null = null,
): ShellRule | null {
  const folded = tool === null ? foldPrefix(prefix) : foldPathPrefix(prefix);
  const matching = rules.filter((rule) => rule.tool === tool && rule.prefix === folded);
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
