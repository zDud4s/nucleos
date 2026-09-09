// §spec alcada-por-projecto
import { useState } from "react";
import type { UseQueryResult } from "@tanstack/react-query";
import { isApiRefusal, type ApiRefusal } from "../data/client";
import {
  useGithubListing,
  useProjectRepo,
  type ProjectRepo,
  type ReadOutcome,
} from "../data/project-github";
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
import { Button, Quiet, RefusalNote, Section } from "../ui";

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
        <Remote projectId={projectId} />
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
 * Open pull requests and the last CI runs, read by the daemon with the token from Credential
 * Manager.
 *
 * **Nothing in this section may ever be blank, and that is the requirement it is built around.**
 * §5.1: *«Sem token ou sem `gh` encontrado, explicado e não em branco — um painel vazio é
 * indistinguível de um repositório sem PRs»*. So there are two questions asked in order and eleven
 * answers between them, and every one of the ten that is not a listing says what is wrong and, where
 * there is one, what to do about it.
 *
 * **The mapping comes first, and it is the fact this section waited on.** A typed read carries the
 * repository it reads as `owner/name`, and until `GET /projects/{id}/github-repo` landed nothing in
 * the daemon could produce one for a project — `Repo::new` was reachable from `ReadOp::from_request`
 * alone, so the repository was always something the caller named. This page could have chained
 * `GET /projects/detect` and parsed a URL into a slug; that would have been the app inventing a
 * mapping the daemon does not hold, and getting it subtly wrong would have shown somebody another
 * repository's pull requests under their project's name. The daemon holds it now.
 *
 * **What comes back is `gh`'s PROSE and it is rendered as prose.** `--json` is in the núcleo's
 * `REFUSED_READ_FLAGS` on purpose, and so are `--limit` and `-L`: `github.rs` grades `pr_list` and
 * `run_list` `ReadsOwn` — the grading that lets a run read them without latching its turn — on the
 * strength of `gh`'s own thirty-row cap, which it calls *"not a performance detail, it is half of
 * this grading"*. Parsing this into a sorted table would mean asking for `--json`, which would mean
 * spending that argument on a layout. What is on screen is what the owner would see at a terminal.
 *
 * **Asked once, never polled.** Each listing is a subprocess and a network call, and a three-second
 * timer would have this window running `gh` for ever to redraw a panel nobody is looking at. The
 * remote does change without anybody here touching it, though — that is what CI is — so there is one
 * control that asks again, and it is the only gesture in this section.
 */
function Remote({ projectId }: { projectId: string }) {
  const mapping = useProjectRepo(projectId);
  const found = mapping.data;
  const repo = found?.state === "known" ? found.repo : null;
  const prs = useGithubListing("pr_list", repo);
  const runs = useGithubListing("run_list", repo);

  if (mapping.isPending) {
    return <p className="text-sm text-text-faint">Asking which repository this project is…</p>;
  }

  // A project the roster has never heard of is the one refusal this read makes, and it is a real
  // error rather than a state: there is nothing to describe.
  if (found === undefined) {
    return isApiRefusal(mapping.error) ? (
      <RefusalNote refusal={mapping.error} sentences={MAPPING_SENTENCES} />
    ) : (
      <Quiet says="the núcleo did not answer which repository this project is">
        The daemon is reachable or this window would be showing nothing at all, so this is the one
        read failing rather than the connection. Reopening the page asks again.
      </Quiet>
    );
  }

  if (found.state !== "known") {
    const why = WHY_NO_REPOSITORY[found.state];
    return (
      <Quiet says={why.says}>
        {why.because}
        {"root" in found ? (
          <>
            {" "}
            The folder is <span className="font-mono">{found.root}</span>.
          </>
        ) : null}
        {"remote" in found ? (
          <>
            {" "}
            <span className="font-mono">origin</span> is{" "}
            <span className="font-mono">{found.remote}</span>.
          </>
        ) : null}
      </Quiet>
    );
  }

  const asking = prs.isFetching || runs.isFetching;

  return (
    <div className="flex flex-col gap-4">
      <p className="flex flex-wrap items-baseline gap-2 text-sm text-text-muted">
        <span className="font-mono text-text">{found.repo}</span>
        {/*
          The URL beside the slug, because they answer different questions. The slug is what the
          daemon sends `gh` at; the URL is what somebody checks it against when the slug is not the
          repository they were expecting — which is the one failure a mapping can have that looks
          like success.
        */}
        <span className="font-mono text-xs text-text-faint">{found.remote}</span>
        <Button
          onClick={() => {
            void prs.refetch();
            void runs.refetch();
          }}
          disabled={asking}
        >
          {asking ? "asking GitHub…" : "ask again"}
        </Button>
      </p>

      <Listing title="Open pull requests" read={prs} />
      <Listing title="The last CI runs" read={runs} />
    </div>
  );
}

/**
 * Why this project has no repository on GitHub, in a sentence and a paragraph.
 *
 * **Five states and five sentences, because five different things are wrong and four of them are
 * fixable in four different places.** This is the table §5.1's rule reduces to: a mapping that
 * answered "no repository" five times would be a panel that is blank with extra steps.
 *
 * None of these is an error. A project in `off` mode has had its root cleared, a checkout can be
 * moved, `set_project_mode` only insists on a repository for `active`, a local-only project is a
 * project, and a project on GitLab is simply not a question this app answers.
 */
const WHY_NO_REPOSITORY: Record<
  Exclude<ProjectRepo["state"], "known">,
  { says: string; because: string }
> = {
  no_root: {
    says: "this project has no folder, so there is no remote to read",
    because:
      "Switching a project off clears the root it was pointed at — the row stays on the roster and the folder is forgotten. Put it back into shadow or active with a folder, and this section fills itself in.",
  },
  root_missing: {
    says: "the folder this project points at is not there",
    because:
      "The núcleo has a root recorded for this project and nothing is at it, which is what a checkout somebody moved or deleted looks like. Nothing here is lost: point the project at where the folder is now.",
  },
  not_a_repository: {
    says: "this project's folder is not a git repository",
    because:
      "That is allowed — only active mode insists on a repository, so a project can be watched in a folder git knows nothing about. There is no origin to read, and so nothing on GitHub to show.",
  },
  no_remote: {
    says: "this repository has no origin",
    because:
      "A local-only project is a project, and this is what one looks like from here rather than a fault. `git remote add origin …` in that folder is the whole of what this section is waiting for.",
  },
  not_github: {
    says: "this project's origin is not a GitHub repository",
    because:
      "The daemon reads github.com and nothing else — its gh is pointed at the public host, so a repository anywhere else is one it could not fetch even if this page asked. There is nothing wrong here; this section just has nothing to say about it.",
  },
};

/** The one refusal `GET /projects/{id}/github-repo` makes, in this page's voice. */
const MAPPING_SENTENCES: Record<string, string> = {
  not_found: "the núcleo has no project by this name.",
  internal: "the núcleo hit an error of its own working out which repository this is.",
};

/**
 * One listing, or the reason there is not one.
 *
 * **Four outcomes and none of them is an empty box.** The daemon refused; `gh` ran and failed; `gh`
 * ran, succeeded and printed nothing; `gh` printed a listing. The third is the one worth naming: a
 * repository with no open pull requests and a `gh` that answered with silence look identical in a
 * panel, which is the sentence §5.1 is built out of, so the empty case says out loud that it is an
 * answer rather than an absence.
 *
 * The text goes in a `pre`. It is a terminal table — columns aligned with spaces — and any element
 * that reflowed it would turn `gh`'s own formatting into noise.
 */
function Listing({ title, read }: { title: string; read: UseQueryResult<ReadOutcome> }) {
  return (
    <div className="flex flex-col gap-2 rounded-lg border border-border bg-surface p-4">
      <p className="text-xs uppercase tracking-wide text-text-faint">{title}</p>
      <ListingBody read={read} />
    </div>
  );
}

function ListingBody({ read }: { read: UseQueryResult<ReadOutcome> }) {
  if (read.isPending) {
    return <p className="text-xs text-text-faint">Asking GitHub…</p>;
  }

  if (read.data === undefined) {
    return isApiRefusal(read.error) ? (
      <RefusalNote refusal={read.error} sentences={sentencesFor(read.error)} />
    ) : (
      <p className="text-xs text-text-muted">
        The núcleo did not answer this read at all. That is the daemon and not GitHub — asking again
        is the whole treatment.
      </p>
    );
  }

  const outcome = read.data;
  // `gh` ran and did not succeed. The núcleo answers 200 for this, and rightly: the request was
  // fine and GitHub's answer was not. `output_tail` is stdout then stderr, which is where the CLI
  // says why — a renamed repository, a token without access to it, a network that is down.
  if (outcome.exit_code !== 0) {
    const said = outcome.output_tail.trim();
    return (
      <div className="flex flex-col gap-1">
        <p className="text-xs text-text-muted">
          gh ran and did not succeed{outcome.exit_code === null ? "" : ` (exit ${outcome.exit_code})`}
          . This is GitHub's answer rather than the núcleo's, and it is printed whole below.
        </p>
        {said === "" ? (
          <p className="text-xs text-text-faint">And it printed nothing at all while failing.</p>
        ) : (
          <pre className="overflow-x-auto whitespace-pre font-mono text-xs text-text-muted">
            {said}
          </pre>
        )}
      </div>
    );
  }

  const listed = outcome.stdout.trim();
  if (listed === "") {
    const said = outcome.output_tail.trim();
    return (
      <p className="text-xs text-text-muted">
        gh answered and listed nothing. {said === "" ? "" : `It said: ${said}`}
      </p>
    );
  }

  return (
    <pre className="overflow-x-auto whitespace-pre font-mono text-xs text-text">{listed}</pre>
  );
}

/**
 * What to do about a refusal from `POST /github/requests`, said on top of what the daemon said.
 *
 * **Built from the refusal rather than written as a constant, because the daemon's own sentence is
 * the better half and `RefusalNote` shows only one.** Its fallback order is page copy, then the
 * shared floor, then the daemon's prose — and the floor has an entry for both statuses this route
 * uses, so page copy that ignored the detail would replace *"gh is not on this machine's PATH"* with
 * *"the part of the núcleo this needs is not available"*. So the detail is quoted and the advice is
 * added to it.
 *
 * **The 503 covers two different faults and this does not guess which.** A switched-off pillar and a
 * missing `gh` are both 503 — the núcleo keeps them apart in words and not in the status — and
 * sniffing the prose to tell them apart would be a `switch` over sentences, which is the one thing
 * `client.ts` says never to build. So the advice names both places to look and the daemon's sentence
 * above it says which.
 */
function sentencesFor(refusal: ApiRefusal): Record<string, string> {
  const said = refusal.detail.trim();
  const advice = ADVICE[refusal.status];
  // **An empty map would have thrown the daemon's sentence away, which is the opposite of the
  // intent.** `RefusalNote` falls back page copy → shared floor → daemon prose, so returning nothing
  // for a status with no advice hands the sentence to the FLOOR, not to the daemon: a 500 read "the
  // núcleo hit an error of its own handling this" in place of "the github task did not finish". The
  // daemon's words are always at least as good, so they are always passed on; the advice, when there
  // is any, is added to them.
  if (said === "") return advice === undefined ? {} : { [refusal.code]: advice };
  return { [refusal.code]: advice === undefined ? said : `${said} — ${advice}` };
}

const ADVICE: Record<number, string> = {
  403: "the token lives in this machine's Credential Manager and the daemon reads it there at every call, so pasting one and asking again is all this needs",
  503: "either the pillar is switched off in .ai/github.yaml or gh is not installed where the daemon can find it; the sentence above says which, and both are fixed and then the daemon restarted",
  504: "GitHub or the network took longer than the núcleo waits; nothing is wrong here that asking again will not settle",
};

/**
 * A section whose own read refused, saying so instead of waiting for ever.
 *
 * **§5.1's rule applied to the three sections below it, which is where it was missing.** *«um painel
 * vazio é indistinguível de um repositório sem PRs»* — and a guard written as `data === undefined`
 * cannot tell "still loading" from "refused", so with `retry: false` (the house default, and argued
 * for at every one of these hooks) the loading line was permanent. Three sections that decide what
 * an autonomous run may do sat reading *"Reading this project's rules…"* for ever, which is the same
 * failure the first section is built to avoid, in the panels where it costs most.
 *
 * `try_github_ops` and `try_land_targets` exist so this can be said: both were given a `Result`
 * rather than swallowing to an empty list, on the argument that to a display `[]` is a positive
 * claim that nothing is declared. A page that then discarded the error threw away the distinction
 * they were built for.
 *
 * The refusal's own words are shown. These routes refuse with a bare 500 and no body, so
 * `RefusalNote`'s floor supplies the sentence — which for an unnamed internal error is the true one.
 */
function ReadFailed({ read, says }: { read: UseQueryResult<unknown>; says: string }) {
  if (isApiRefusal(read.error)) {
    return <RefusalNote refusal={read.error} sentences={{ not_found: NO_SUCH_PROJECT }} />;
  }
  return (
    <Quiet says={`the núcleo did not answer ${says}`}>
      The daemon is reachable or this window would be showing nothing at all, so this is the one read
      failing rather than the connection. Nothing here is lost — the tables are the núcleo's, and it
      is still enforcing them; this is only the picture of them. Reopening the page asks again.
    </Quiet>
  );
}

const NO_SUCH_PROJECT = "the núcleo has no project by this name.";

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

  if (catalogue.isPending || mine.isPending) {
    return <p className="text-sm text-text-faint">Reading what this project may do…</p>;
  }

  if (catalogue.data === undefined || mine.data === undefined) {
    // Whichever of the two refused. The catalogue first, because a page that cannot say what MAY be
    // declared cannot draw this section at all, while a missing `mine` only costs the ticks.
    return (
      <ReadFailed
        read={catalogue.data === undefined ? catalogue : mine}
        says="what this project may do on its own"
      />
    );
  }

  const declared = new Set(mine.data);
  const reads = catalogue.data.filter((op) => op.half === "read");
  const actions = catalogue.data.filter((op) => op.half === "action");
  const pending = declare.isPending || forget.isPending;
  // Declared kinds this build has no operation for. `try_github_ops` serves the table RAW — no
  // narrowing, deliberately — so these arrive, and rendering only from the catalogue dropped them.
  const built = new Set(catalogue.data.map((op) => op.kind));
  const stranded = mine.data.filter((kind) => !built.has(kind)).sort();

  function toggle(kind: string, on: boolean) {
    if (on) declare.mutate({ projectId, opKind: kind });
    else forget.mutate({ projectId, opKind: kind });
  }

  return (
    <div className="flex flex-col gap-4">
      <OpHalf
        title="Reads"
        /*
          Qualified, because unqualified it was an overstatement on a page whose subject is who
          decides. `GithubRuntime::policy_for_project` returns `Policy::empty()` outright when the
          pillar is off — pinned by `a_switched_off_pillar_stays_off_whatever_the_project_declared` —
          and `post_project_github_op` has no `enabled` check, so an owner can declare reads on a
          machine where nothing will ever consult them. It errs safe, and it is still a promise the
          núcleo has not made. The condition is named rather than a fourth read added to this page
          for one sentence; section 1 already reports the pillar's state the moment anything is asked
          of it.
        */
        says="A read declared here is consulted at the next decision: the classifier already asks this list when an agent writes gh in the Bash tool. It binds only while the GitHub pillar is on — with enabled: false in .ai/github.yaml the núcleo uses an empty policy whatever a project has declared, and nothing on this page overrides that."
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

      {stranded.length === 0 ? null : (
        /*
          Rows this build has no operation for at all.

          Not the same case as an operation outside the ceilings: those are still in the catalogue,
          with a `half` to file them under and a name this daemon knows. These are names the binary
          no longer builds — a renamed operation, one removed between versions — so they appear in
          `mine.data` and in neither half, and the old rendering dropped them silently. A row the
          owner cannot see is a row they cannot remove, and it stays in the table for ever.

          `delete_project_github_op` takes any kind, which is what makes this block possible: its own
          doc says withdrawing narrows and an operation stored before the ceilings moved still has to
          be removable.
        */
        <div className="flex flex-col gap-2 rounded-lg border border-border bg-surface p-4">
          <p className="text-xs uppercase tracking-wide text-text-faint">No longer built</p>
          <p className="max-w-3xl text-xs text-text-muted">
            This project's table names operations this daemon does not build. They decide nothing —
            the núcleo narrows them away before it grants anything — and the rows are still yours to
            remove.
          </p>
          <ul className="flex flex-col gap-1">
            {stranded.map((kind) => (
              <li
                key={kind}
                aria-label={`operation ${kind}`}
                className="flex flex-wrap items-baseline gap-2 text-sm"
              >
                <span className="rounded-pill border border-border bg-surface-sunken px-2 py-0.5 font-mono text-xs text-text-muted">
                  {kind}
                </span>
                <span className="ml-auto">
                  <Button variant="quiet" disabled={pending} onClick={() => toggle(kind, false)}>
                    withdraw
                  </Button>
                </span>
              </li>
            ))}
          </ul>
        </div>
      )}

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
                    A `span` and never a checkbox — the `Settings` chip's argument, applied to an
                    operation instead of to a class. The compiled ceilings do not admit this one, so
                    there is nothing for a box to be wired to, and a box that refused would be a lie
                    about who decides.
                  */}
                  <span className="rounded-pill border border-border bg-surface-sunken px-2 py-0.5 font-mono text-xs text-text-muted">
                    {op.kind}
                  </span>
                  <span className="text-xs text-text-faint">
                    {declared.has(op.kind) ? OUTSIDE_AND_DECLARED : OUTSIDE_THE_CEILING}
                  </span>
                  {/*
                    The one legal gesture, and only when there is a row to remove.
                    `delete_project_github_op` has no declarability check, and says why: *"withdrawing
                    narrows, and an operation stored before the ceilings moved still has to be
                    removable"*. `GET .../github-ops` serves the raw table for the same reason, so a
                    row outside the ceilings arrives here on purpose. Drawing it with no control at
                    all left the owner holding a row they could see was there and could not remove —
                    and, worse, captioned as though nothing were stored.
                  */}
                  {declared.has(op.kind) ? (
                    <span className="ml-auto">
                      <Button variant="quiet" disabled={pending} onClick={() => onToggle(op.kind, false)}>
                        withdraw
                      </Button>
                    </span>
                  ) : null}
                </>
              )}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

const OUTSIDE_THE_CEILING = "outside the compiled ceiling — nothing on this machine can turn it on";

/**
 * The row that used to be a contradiction: stored, and captioned as though it were not.
 *
 * `Policy::for_project` narrows it away, so the AUTHORITY is right and this operation does not run —
 * but the row is in the project's table, the owner cannot see that from the old caption, and the
 * withdraw is the only thing that makes the table agree with the page again.
 */
const OUTSIDE_AND_DECLARED =
  "declared here, and outside the compiled ceiling — it does not run, and the row is still yours to withdraw";

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

  if (rules.isPending) {
    return <p className="text-sm text-text-faint">Reading this project's rules…</p>;
  }

  if (rules.data === undefined) {
    return <ReadFailed read={rules} says="what this project's worktrees may run" />;
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
              <span className="ml-auto">
                <Button variant="quiet" disabled={pending} onClick={() => onFlip(rule)}>
                {rule.verdict === "allow" ? "refuse it instead" : "allow it instead"}
                </Button>
              </span>
              <Button variant="quiet" disabled={pending} onClick={() => onForget(rule.prefix)}>
                forget
              </Button>
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
 *
 * **The default comes from the route that owns landings, and never from the checkout's HEAD.** This
 * panel used to read `GET /projects/{id}/branches` for it — `inspect::Branches::integration`, which
 * is `current_branch(project_root)` — and that field's own doc in the núcleo says the 2026-08-27
 * design killed that read *"precisely because a checkout parked on the wrong branch silently
 * redirected every landing"*. Any project whose main clone is checked out onto a feature branch made
 * both sentences here false in both directions: the branch shown was refused by name, and the branch
 * that is admissible appeared nowhere. It is the defect `land.rs` exists to abolish, and a page is
 * not a safe place to reintroduce it.
 */
function LandTargets({ projectId }: { projectId: string }) {
  const landing = useProjectLandTargets(projectId);
  const open = useDeclareLandTarget();
  const close = useForgetLandTarget();
  const [branch, setBranch] = useState("");

  const refused =
    (open.isError && isApiRefusal(open.error) ? open.error : null) ??
    (close.isError && isApiRefusal(close.error) ? close.error : null);

  if (landing.isPending) {
    return <p className="text-sm text-text-faint">Reading where this project lands…</p>;
  }

  if (landing.data === undefined) {
    return <ReadFailed read={landing} says="where this project's work lands" />;
  }

  const { integration, targets } = landing.data;

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
          {integration.state === "unknown" ? (
            <span className="text-xs text-text-faint">{integration.why}</span>
          ) : (
            <>
              <span className="font-mono text-xs text-text">{integration.branch}</span>
              {/*
                Only the `declared` arm gets the claim. A fact and not a row: the integration branch
                needs no entry in the table to stay admissible, so there is nothing to close and no
                button is offered — and in the other two arms nothing lands by default at all, so
                borrowing the caption would assert exactly what the núcleo would refuse.
              */}
              <span className="text-xs text-text-faint">{CAPTION[integration.state]}</span>
            </>
          )}
        </li>
        {targets.map((target) => (
          <li
            key={target}
            aria-label={`land target ${target}`}
            className="flex flex-wrap items-baseline gap-2 text-sm"
          >
            <span className="font-mono text-xs text-text">{target}</span>
            <span className="ml-auto">
              <Button
                variant="quiet"
                disabled={close.isPending}
                onClick={() => close.mutate({ projectId, branch: target })}
              >
                close
              </Button>
            </span>
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

/**
 * What each arm of the default means, in the one line beside the branch name.
 *
 * **Only `declared` gets the claim, and that is the point of having three.** A branch name reads as
 * equally admissible in all three, so the caption is the only thing on screen that separates *this
 * is where work goes* from *this is what is recorded and it does not resolve* and *this is what
 * would be derived if anything landed*. `unknown` has no entry because it names no branch — it
 * renders the daemon's own sentence instead.
 */
const CAPTION: Record<"declared" | "stale" | "derived", string> = {
  declared: "the integration branch — always admissible, with or without a row",
  stale:
    "declared as the integration branch, and git cannot find that ref — nothing lands by default until it is corrected",
  derived:
    "where a landing would go, derived from the repository. It has not been written down yet, so the first landing records it",
};

const TARGET_SENTENCES: Record<string, string> = {
  no_such_project: "the núcleo has no project by this name.",
  no_such_target:
    "no target of that name was opened here. The integration branch is admissible without a row, so there is never one of those to close.",
  internal: "the núcleo hit an error of its own writing it down.",
};
