// §spec alcada-por-projecto
import { useState } from "react";
import { isApiRefusal } from "../data/client";
import { useProjectBranches } from "../data/project-git";
import {
  declaredRule,
  foldPrefix,
  useDeclarableGithubOps,
  useDeclareGithubOp,
  useDeclareLandTarget,
  useDeclareShellRule,
  useForgetGithubOp,
  useForgetLandTarget,
  useForgetShellRule,
  useProjectGithubOps,
  useProjectLandTargets,
  useProjectShellRules,
  type DeclarableOp,
  type Note,
  type ShellRule,
  type Verdict,
} from "../data/project-policy";
import { Quiet, RefusalNote, Section } from "../ui";

/**
 * "What may this project do without asking?"
 *
 * The fifth mode, and it earns the tab by the rule the other four are held to: it has a subject of
 * its own — this project's authority, which is three tables and not a setting — and a shape of its
 * own, four sections read top to bottom rather than a grid of panels. §5 of the design fixes the
 * order and the reason for it: the remote first, because that is what somebody arrives to look at,
 * and the government under it.
 *
 * **Everything below the first section is a live autonomy control and not a configuration edit.**
 * `hooks.rs` reads the shell table per tool call, with no cache and no restart, so a rule declared
 * here binds the very next tool call of every in-flight run of this project. That is the sentence
 * the page opens with, because a form that read as a preferences pane would be inviting somebody to
 * "try something" against runs that are working right now.
 *
 * **Not polled**, like the data layer under it: a declaration changes when a person edits it and at
 * no other time, and a timer here would ask three questions every three seconds to watch a list
 * only this window can change.
 */

export interface ModeGithubProps {
  projectId: string;
}

export function ModeGithub({ projectId }: ModeGithubProps) {
  return (
    <div className="flex flex-col gap-8">
      <p className="max-w-3xl text-sm text-text-muted">
        What this project may do on its own. The núcleo reads these tables at the moment it decides,
        with no cache and no restart, so a rule written here binds the very next tool call of every
        run already working in {projectId} — this is a live control and not a preference.
      </p>

      <Section label="The remote">
        <Remote />
      </Section>

      <Section label="What runs on its own">
        <AutonomousOps projectId={projectId} />
      </Section>

      <Section label="What the worktrees may run">
        <ShellRules projectId={projectId} />
      </Section>

      <Section label="Where the work lands">
        <LandTargets projectId={projectId} />
      </Section>
    </div>
  );
}

/* ------------------------------------------------------------ 1. the remote -- */

/**
 * Open pull requests and the last CI runs — explained, because they cannot yet be fetched.
 *
 * §5.1 asks for the typed reads `pr_list` and `run_list`, through the daemon, with the token from
 * Credential Manager, and it names the failure this section must avoid: *«um painel vazio é
 * indistinguível de um repositório sem PRs»*. That rule cuts both ways, and it is why nothing is
 * drawn here rather than a panel that stays empty for a reason nobody can see.
 *
 * **What is missing is one fact and not one route.** `POST /github/requests` exists, admits the
 * control token this window holds, and runs a read the moment it is asked. Its body is an operation,
 * every read operation carries a `repo` — `owner/name` — and *nothing tells this app which
 * repository a project is*. In the núcleo `Repo::new` is reached from `ReadOp::from_request` alone,
 * so the repository is always something the CALLER names; `vcs::resolve_repo` answers with a folder
 * on disk, which is a different question. `GET /projects/detect` reports a git remote, but it takes
 * a path and belongs to the wizard that adds a project, and chaining it here to parse a URL into a
 * slug would be this app inventing a mapping the daemon does not hold.
 *
 * So the honest sentence is the one below, in the voice §5.1 asks for on a machine with no token or
 * no `gh` found: say what belongs here, and say why it is not here yet.
 */
function Remote() {
  return (
    <Quiet says="not wired yet">
      Open pull requests and the last CI runs belong here, read by the daemon through the typed
      operations <span className="font-mono">pr_list</span> and{" "}
      <span className="font-mono">run_list</span> with the token from Credential Manager. Nothing on
      this machine can ask for them yet: a typed read carries the repository it reads —{" "}
      <span className="font-mono">owner/name</span> — and no route tells this app which repository a
      project is. Until one does, this section says so. A panel that guessed would be
      indistinguishable from a repository with no open pull requests, which is the one thing this
      section must never look like.
    </Quiet>
  );
}

/* ------------------------------------------------ 2. what runs on its own -- */

/**
 * The single list of GitHub operations, and the ceiling drawn around it.
 *
 * **What is outside the ceiling is a fact and never a control.** `Settings.tsx` already applies this
 * rule to action classes — its chips are `span`s, and its header says why: *"nothing in the núcleo
 * lets a person grant a class by hand and a chip that looked clickable would be a lie about who
 * decides"*. The same reading holds here, and the design says it in the same words: there is no box
 * to switch `api_read` on because there is no way to switch it on, and `declarable: false` is a fact
 * about the build that no route, no file and no owner can change. So an undeclarable operation gets
 * no checkbox, no button and nothing that can be pressed — only its name and the reason it is out of
 * reach.
 *
 * **The two halves are not drawn as one list**, because they are not in force in the same way. A
 * declared READ is consulted through the Bash door at the next decision. A declared ACTION is
 * recorded and inert — the route stores it and answers the same 204, and a later step wires it.
 * Telling an owner their declared action is in force would be describing a step that has not landed.
 */
function AutonomousOps({ projectId }: { projectId: string }) {
  const catalogue = useDeclarableGithubOps();
  const mine = useProjectGithubOps(projectId);
  const declare = useDeclareGithubOp();
  const forget = useForgetGithubOp();

  const refused =
    (declare.isError && isApiRefusal(declare.error) ? declare.error : null) ??
    (forget.isError && isApiRefusal(forget.error) ? forget.error : null);

  if (catalogue.data === undefined || mine.data === undefined) {
    return <p className="text-sm text-text-faint">Reading what this project may do…</p>;
  }

  const declared = new Set(mine.data);
  const reads = catalogue.data.filter((op) => op.half === "read");
  const actions = catalogue.data.filter((op) => op.half === "action");
  const pending = declare.isPending || forget.isPending;

  function toggle(kind: string, on: boolean) {
    if (on) declare.mutate({ projectId, opKind: kind });
    else forget.mutate({ projectId, opKind: kind });
  }

  return (
    <div className="flex flex-col gap-4">
      <OpHalf
        title="Reads"
        says="A read declared here is consulted at the next decision: the classifier already asks this list when an agent writes gh in the Bash tool."
        ops={reads}
        declared={declared}
        pending={pending}
        onToggle={toggle}
      />
      <OpHalf
        title="Actions"
        says="An action declared here is recorded and inert. The typed door still files one for your approval; wiring this list to it is a later step, and nothing on this page puts an action in force today."
        ops={actions}
        declared={declared}
        pending={pending}
        onToggle={toggle}
      />

      {refused !== null ? (
        /*
          No page copy for `undeclarable_op`. Its detail names the operation AND lists what the
          ceilings admit, and that list reaches the wire nowhere else — a sentence written here
          would be strictly less than what the daemon already said.
        */
        <RefusalNote refusal={refused} sentences={OP_SENTENCES} />
      ) : null}
    </div>
  );
}

const OP_SENTENCES: Record<string, string> = {
  kill_switch:
    "the emergency stop is engaged, and granting an operation widens what runs on its own. Withdrawing one is never blocked by it.",
  no_such_project: "the núcleo has no project by this name.",
  no_such_op: "this project had not declared that one.",
  internal: "the núcleo hit an error of its own writing it down.",
};

function OpHalf({
  title,
  says,
  ops,
  declared,
  pending,
  onToggle,
}: {
  title: string;
  says: string;
  ops: DeclarableOp[];
  declared: Set<string>;
  pending: boolean;
  onToggle: (kind: string, on: boolean) => void;
}) {
  return (
    <div className="flex flex-col gap-2 rounded-lg border border-border bg-surface p-4">
      <p className="text-xs uppercase tracking-wide text-text-faint">{title}</p>
      <p className="max-w-3xl text-xs text-text-muted">{says}</p>
      {ops.length === 0 ? (
        <p className="text-xs text-text-faint">This daemon builds none of this half.</p>
      ) : (
        <ul className="flex flex-col gap-1">
          {ops.map((op) => (
            <li
              key={op.kind}
              aria-label={`operation ${op.kind}`}
              className="flex flex-wrap items-baseline gap-2 text-sm"
            >
              {op.declarable ? (
                <label className="flex items-center gap-1.5">
                  <input
                    type="checkbox"
                    checked={declared.has(op.kind)}
                    disabled={pending}
                    onChange={(event) => onToggle(op.kind, event.target.checked)}
                  />
                  <span className="font-mono text-xs text-text">{op.kind}</span>
                </label>
              ) : (
                <>
                  {/*
                    A `span` and never a control — the `Settings` chip's argument, applied to an
                    operation instead of to a class. The compiled ceilings do not admit this one, so
                    there is nothing for a box to be wired to, and a box that refused would be a lie
                    about who decides.
                  */}
                  <span className="rounded-pill border border-border bg-surface-sunken px-2 py-0.5 font-mono text-xs text-text-muted">
                    {op.kind}
                  </span>
                  <span className="text-xs text-text-faint">
                    outside the compiled ceiling — nothing on this machine can turn it on
                  </span>
                </>
              )}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

/* -------------------------------------- 3. what the worktrees may run -- */

/**
 * The two lists, `allow` and `deny`, with the rule that orders them written on the page.
 *
 * §5.3 asks for the precedence *«escrita na página e não só no código»*, and it is not decoration:
 * the two lists are not mirror images, and somebody reading them as two ends of one switch will
 * write an `allow` expecting it to lift a refusal. So the rule is a paragraph above the lists rather
 * than a tooltip on them.
 *
 * **One row per rule, grouped here rather than served grouped.** `useProjectShellRules` answers
 * `ShellRule[]` — prefix, verdict, note, and the day it was first written down — because the note is
 * the column the whole table exists for and a pair of prefix lists could not carry one.
 */
function ShellRules({ projectId }: { projectId: string }) {
  const rules = useProjectShellRules(projectId);
  const declare = useDeclareShellRule();
  const forget = useForgetShellRule();

  const refused =
    (declare.isError && isApiRefusal(declare.error) ? declare.error : null) ??
    (forget.isError && isApiRefusal(forget.error) ? forget.error : null);

  if (rules.data === undefined) {
    return <p className="text-sm text-text-faint">Reading this project's rules…</p>;
  }

  const rows = rules.data;
  const allow = rows.filter((rule) => rule.verdict === "allow");
  const deny = rows.filter((rule) => rule.verdict === "deny");
  const pending = declare.isPending || forget.isPending;

  /**
   * Change a rule's verdict and KEEP its justification.
   *
   * The route rewrites `verdict` and `note` from what it is sent, and does it deliberately — a note
   * that fell back to the stored one could never be removed. So the note has to be resent, and it is
   * read off the row already on screen. A rule that carried none is `{ erase: true }`, which is the
   * honest way to say there was nothing to keep, and never an empty `write`.
   */
  function flip(rule: ShellRule) {
    const note: Note = rule.note === null ? { erase: true } : { write: rule.note };
    declare.mutate({
      projectId,
      prefix: rule.prefix,
      verdict: rule.verdict === "allow" ? "deny" : "allow",
      note,
    });
  }

  return (
    <div className="flex flex-col gap-4">
      {/*
        §4.2's ordering, in the words somebody editing these lists needs. Three sentences and not
        one, because the middle one is the property that makes widening defensible and it is the one
        nobody guesses.
      */}
      <p className="max-w-3xl text-sm text-text-muted">
        <span className="font-mono text-xs text-text">deny</span> beats{" "}
        <span className="font-mono text-xs text-text">allow</span>, and beats a permission compiled
        into the núcleo. An <span className="font-mono text-xs text-text">allow</span> widens only
        what the classifier would otherwise have <em>asked</em> about — it never lifts a refusal, so{" "}
        <span className="font-mono text-xs text-text">rm</span> written here leaves{" "}
        <span className="font-mono text-xs text-text">rm -rf /</span> refused, and a line carrying{" "}
        <span className="font-mono text-xs text-text">$( )</span> stays refused whatever is on these
        lists. A project with no rules at all classifies exactly as it did before there were any.
      </p>

      <RuleList
        title="Allowed"
        says="Runs without stopping to ask, in this project's worktrees."
        rows={allow}
        pending={pending}
        onFlip={flip}
        onForget={(prefix) => forget.mutate({ projectId, prefix })}
      />
      <RuleList
        title="Refused"
        says="Never runs here, whatever the compiled lists would have said."
        rows={deny}
        pending={pending}
        onFlip={flip}
        onForget={(prefix) => forget.mutate({ projectId, prefix })}
      />

      {/*
        The one mutation, handed down rather than made again inside the form. `useDeclareShellRule`
        is one hook because the núcleo has one operation — the identity of a rule is its folded
        prefix, so declaring and re-verdicting are the same POST — and two `useMutation` calls would
        be two independent states over it: a refusal earned by the form would land on an instance
        this section is not reading, and the note below would never appear.
      */}
      <DeclareRule projectId={projectId} rows={rows} declare={declare} />

      {refused !== null ? (
        /*
          No page copy for `unmatchable_prefix`. Its detail names the prefix, says why an `allow` of
          that shape would be stored and never fire, and tells the owner that the same prefix
          declared as a `deny` WOULD be enforced — which is the next thing they want to do, in the
          núcleo's own words.
        */
        <RefusalNote refusal={refused} sentences={RULE_SENTENCES} />
      ) : null}
    </div>
  );
}

const RULE_SENTENCES: Record<string, string> = {
  /*
    The stop refuses an `allow` and never a `deny`, and the second half is the part worth writing:
    the daemon sends this refusal with no prose at all, and the shared floor's sentence would leave
    somebody believing the stop has closed the whole page.
  */
  kill_switch:
    "the emergency stop is engaged, so nothing here may widen what runs on its own. Declaring the same prefix as a refusal is not blocked — the stop never stands in the way of narrowing.",
  empty_prefix: "a rule has to name a prefix.",
  no_such_project: "the núcleo has no project by this name.",
  no_such_rule: "no rule of that name was declared here.",
  internal: "the núcleo hit an error of its own writing the rule.",
};

function RuleList({
  title,
  says,
  rows,
  pending,
  onFlip,
  onForget,
}: {
  title: string;
  says: string;
  rows: ShellRule[];
  pending: boolean;
  onFlip: (rule: ShellRule) => void;
  onForget: (prefix: string) => void;
}) {
  return (
    <div className="flex flex-col gap-2 rounded-lg border border-border bg-surface p-4">
      <p className="text-xs uppercase tracking-wide text-text-faint">{title}</p>
      <p className="text-xs text-text-muted">{says}</p>
      {rows.length === 0 ? (
        <p className="text-xs text-text-faint">None declared.</p>
      ) : (
        <ul className="flex flex-col gap-1">
          {rows.map((rule) => (
            <li
              key={rule.prefix}
              aria-label={`rule ${rule.prefix}`}
              className="flex flex-wrap items-baseline gap-2 text-sm"
            >
              {/*
                The FOLDED spelling, which is what is stored and what is enforced. Echoing what
                somebody typed would be showing them a rule the classifier has never heard of.
              */}
              <span className="font-mono text-xs text-text">{rule.prefix}</span>
              {rule.note === null ? (
                <span className="text-xs text-text-faint">no justification</span>
              ) : (
                <span className="text-xs text-text-muted">{rule.note}</span>
              )}
              <span className="text-xs text-text-faint" title={`${rule.created_at} UTC`}>
                {/*
                  "declared", and deliberately not "edited". `DO UPDATE` sets `verdict` and `note`
                  and leaves `created_at` alone, so a rule re-verdicted this morning still carries
                  the day somebody first wrote it down — captioning it as a change would invent a
                  fact the daemon does not hold.
                */}
                declared {declaredDay(rule.created_at)}
              </span>
              <button
                type="button"
                disabled={pending}
                onClick={() => onFlip(rule)}
                className="ml-auto text-xs text-text-faint underline-offset-2 hover:underline disabled:opacity-40"
              >
                {rule.verdict === "allow" ? "refuse it instead" : "allow it instead"}
              </button>
              <button
                type="button"
                disabled={pending}
                onClick={() => onForget(rule.prefix)}
                className="text-xs text-text-faint underline-offset-2 hover:underline disabled:opacity-40"
              >
                forget
              </button>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

/**
 * PURE: the day half of the daemon's `datetime('now')` text.
 *
 * `2026-03-14 09:41:00` becomes `2026-03-14`. A split and not a parse: the column carries SQLite's
 * space-separated UTC spelling rather than RFC 3339, which `new Date(…)` does not read portably, and
 * a shell that reformatted it would be claiming a timezone the daemon never stated. Text that does
 * not look like that is passed through whole, because showing it unchanged is the one answer that
 * cannot be wrong.
 */
function declaredDay(createdAt: string): string {
  return createdAt.split(" ")[0] ?? createdAt;
}

/**
 * Declaring a rule, and re-declaring one that exists.
 *
 * **The identity of a rule is its FOLDED prefix**, so this form folds before it looks: asking with
 * the typed spelling is how a form offers to create a rule that already exists and then overwrites
 * it without saying so. What will be stored is previewed under the box for the same reason.
 *
 * **And the note is the trap.** A second declaration of a prefix rewrites its note from what is
 * sent, so submitting this form with the box empty ERASES the justification that was there. The form
 * says so in front of the person about to do it, and offers the existing note back — which
 * `declaredRule` can hand over because the row carries it.
 */
function DeclareRule({
  projectId,
  rows,
  declare,
}: {
  projectId: string;
  rows: ShellRule[];
  declare: ReturnType<typeof useDeclareShellRule>;
}) {
  const [prefix, setPrefix] = useState("");
  const [note, setNote] = useState("");

  const folded = foldPrefix(prefix);
  const existing = declaredRule(rows, prefix);
  const ready = folded !== "";

  function submit(verdict: Verdict) {
    declare.mutate(
      {
        projectId,
        prefix,
        verdict,
        // Two operations and two members, because the route cannot be told "leave the note alone".
        note: note.trim() === "" ? { erase: true } : { write: note.trim() },
      },
      {
        // Cleared only on success, so a refused declaration leaves what was typed in front of the
        // person who typed it — the rule `Commands` already follows.
        onSuccess: () => {
          setPrefix("");
          setNote("");
        },
      },
    );
  }

  return (
    <div className="flex flex-col gap-2 rounded-lg border border-border bg-surface p-4">
      <p className="text-xs uppercase tracking-wide text-text-faint">Declare a prefix</p>

      <div className="flex flex-wrap gap-2">
        <input
          aria-label="Command prefix"
          placeholder="bash scripts/gates.sh"
          value={prefix}
          spellCheck={false}
          onChange={(event) => setPrefix(event.target.value)}
          className="min-w-48 flex-1 rounded-md border border-border bg-surface-sunken px-2 py-1 font-mono text-sm text-text"
        />
        <input
          aria-label="Why it is here"
          placeholder="why this is here"
          value={note}
          spellCheck={false}
          onChange={(event) => setNote(event.target.value)}
          className="min-w-48 flex-1 rounded-md border border-border bg-surface-sunken px-2 py-1 text-sm text-text"
        />
      </div>

      {/*
        What the núcleo will actually store, shown only where it differs from what was typed. The
        fold happens on the way IN — whitespace collapsed, ASCII lower-cased — so a form that could
        not say this is a form that surprises people.
      */}
      {folded !== "" && folded !== prefix ? (
        <p className="text-xs text-text-faint">
          stored and enforced as <span className="font-mono text-text-muted">{folded}</span>
        </p>
      ) : null}

      {existing !== null ? (
        <p className="text-xs text-tone-paused-fg">
          <span className="font-mono">{existing.prefix}</span> is already declared as{" "}
          {existing.verdict === "allow" ? "allowed" : "refused"}, so this replaces its verdict and
          its justification rather than adding a second rule.
          {existing.note !== null && note.trim() === "" ? (
            <>
              {" "}
              With the justification box empty, declaring it again <strong>erases</strong> the one it
              carries.{" "}
              <button
                type="button"
                onClick={() => setNote(existing.note ?? "")}
                className="underline underline-offset-2"
              >
                keep its justification
              </button>
            </>
          ) : null}
        </p>
      ) : null}

      <div className="flex flex-wrap items-center gap-2">
        <button
          type="button"
          disabled={!ready || declare.isPending}
          onClick={() => submit("allow")}
          className="rounded-md border border-border px-3 py-1.5 text-xs text-text enabled:hover:border-border-strong disabled:opacity-40"
        >
          allow it here
        </button>
        <button
          type="button"
          disabled={!ready || declare.isPending}
          onClick={() => submit("deny")}
          className="rounded-md border border-border px-3 py-1.5 text-xs text-text enabled:hover:border-border-strong disabled:opacity-40"
        >
          refuse it here
        </button>
        <span className="text-xs text-text-faint">
          A refusal may take a shape an allow may not — a pipe, a redirection, an{" "}
          <span className="font-mono">-exec</span> — because a refusal answers at the whole line and
          needs no shape the classifier can read.
        </span>
      </div>
    </div>
  );
}

/* -------------------------------------------- 4. where the work lands -- */

/**
 * The integration branch, and the branches a `--land` may name besides it.
 *
 * **The integration branch is admissible with no row, and is drawn as a fact.** It is never in the
 * table — an empty table means "nowhere but the usual place" and not "nowhere" — so a close button
 * beside it would be a control whose only possible answer is `no_such_target`. That is the reading
 * section 2 takes about an operation outside the ceiling, one section up.
 *
 * **A branch that does not exist yet is accepted, and this form does not "help" by checking.** The
 * branch is often made by the very run that will land into it; whether it exists is
 * `land::resolve_target`'s question, asked at the moment of landing, where the refusal has somebody
 * to tell. What the route checks here is the NAME.
 */
function LandTargets({ projectId }: { projectId: string }) {
  const branches = useProjectBranches(projectId);
  const targets = useProjectLandTargets(projectId);
  const open = useDeclareLandTarget();
  const close = useForgetLandTarget();
  const [branch, setBranch] = useState("");

  const refused =
    (open.isError && isApiRefusal(open.error) ? open.error : null) ??
    (close.isError && isApiRefusal(close.error) ? close.error : null);

  if (targets.data === undefined) {
    return <p className="text-sm text-text-faint">Reading where this project lands…</p>;
  }

  const integration = branches.data?.integration ?? null;

  return (
    <div className="flex flex-col gap-2 rounded-lg border border-border bg-surface p-4">
      <p className="max-w-3xl text-xs text-text-muted">
        <span className="font-mono text-text">nucleos-core --land &lt;branch&gt;</span> sends what is
        in the worktree it is called from. The destination has to be one of these; a name that is not
        is refused, and the refusal lists what would have been admissible.
      </p>

      <ul className="flex flex-col gap-1">
        <li
          aria-label="land target the integration branch"
          className="flex flex-wrap items-baseline gap-2 text-sm"
        >
          {integration === null ? (
            <span className="text-xs text-text-faint">
              {branches.data === undefined
                ? "reading the integration branch…"
                : "this project is on a detached HEAD, so the daemon has no integration branch to name"}
            </span>
          ) : (
            <>
              <span className="font-mono text-xs text-text">{integration}</span>
              {/*
                A fact and not a row: the integration branch needs no entry in the table to stay
                admissible, so there is nothing here to close and no button is offered.
              */}
              <span className="text-xs text-text-faint">
                the integration branch — always admissible, with or without a row
              </span>
            </>
          )}
        </li>
        {targets.data.map((target) => (
          <li
            key={target}
            aria-label={`land target ${target}`}
            className="flex flex-wrap items-baseline gap-2 text-sm"
          >
            <span className="font-mono text-xs text-text">{target}</span>
            <button
              type="button"
              disabled={close.isPending}
              onClick={() => close.mutate({ projectId, branch: target })}
              className="ml-auto text-xs text-text-faint underline-offset-2 hover:underline disabled:opacity-40"
            >
              close
            </button>
          </li>
        ))}
      </ul>

      <div className="flex flex-wrap items-center gap-2">
        <input
          aria-label="Another landing target"
          placeholder="release/next"
          value={branch}
          spellCheck={false}
          onChange={(event) => setBranch(event.target.value)}
          className="min-w-48 flex-1 rounded-md border border-border bg-surface-sunken px-2 py-1 font-mono text-sm text-text"
        />
        <button
          type="button"
          disabled={branch.trim() === "" || open.isPending}
          onClick={() =>
            open.mutate({ projectId, branch: branch.trim() }, { onSuccess: () => setBranch("") })
          }
          className="rounded-md border border-border px-3 py-1.5 text-xs text-text enabled:hover:border-border-strong disabled:opacity-40"
        >
          open it
        </button>
      </div>

      {refused !== null ? (
        /*
          No page copy for `unusable_branch`: its detail says which part of the spelling was refused
          — an empty name, a leading dash, whitespace — and that is what somebody has to fix.
        */
        <RefusalNote refusal={refused} sentences={TARGET_SENTENCES} />
      ) : null}
    </div>
  );
}

const TARGET_SENTENCES: Record<string, string> = {
  no_such_project: "the núcleo has no project by this name.",
  no_such_target:
    "no target of that name was opened here. The integration branch is admissible without a row, so there is never one of those to close.",
  internal: "the núcleo hit an error of its own writing it down.",
};
