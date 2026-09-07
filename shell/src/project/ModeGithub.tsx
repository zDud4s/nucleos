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
  foldPathPrefix,
  foldPrefix,
  useDeclarableGitOps,
  useDeclarableGithubOps,
  useDeclareGitOp,
  useDeclareGithubOp,
  useDeclareLandTarget,
  useDeclareShellRule,
  useForgetGitOp,
  useForgetGithubOp,
  useForgetLandTarget,
  useForgetShellRule,
  useProjectGitOps,
  useProjectGithubOps,
  useProjectLandTargets,
  useProjectShellRules,
  type DeclarableGitOp,
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
 * its own — this project's authority, which is four tables and not a setting — and a shape of its
 * own, five sections read top to bottom rather than a grid of panels. §5 of
 * `.ai/specs/2026-09-03-alcada-por-projecto-design.md` and §7 of
 * `.ai/specs/2026-09-06-git-ops-declaraveis-design.md` fix the order and the reason for it: the
 * remote first, because that is what somebody arrives to look at, and the government under it.
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

      <Section label="What the queue may do on its own">
        <GitOps projectId={projectId} />
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
                <button
                  type="button"
                  disabled={pending}
                  onClick={() => toggle(kind, false)}
                  className="ml-auto text-xs text-text-faint underline-offset-2 hover:underline disabled:opacity-40"
                >
                  withdraw
                </button>
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
                    <button
                      type="button"
                      disabled={pending}
                      onClick={() => onToggle(op.kind, false)}
                      className="ml-auto text-xs text-text-faint underline-offset-2 hover:underline disabled:opacity-40"
                    >
                      withdraw
                    </button>
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

/* -------------------------------- 3. what the queue may do on its own -- */

/**
 * The git operations an autonomous run may hand to the shared queue as already consented.
 *
 * **One list and no GitHub halves.** Every row is a write the queue performs in the same way; a
 * `half` would suggest the live-read versus inert-action distinction that belongs only to GitHub.
 * The project list is still served raw, so a declaration this build no longer constructs is drawn
 * below the catalogue and remains withdrawable instead of disappearing from its owner's view.
 *
 * **The paragraph is part of the control.** A tick changes who the queue waits for, not who runs the
 * command, and only after both parsing and shell-rule precedence have admitted that path. The
 * spellings and rebase caveat are written beside the boxes because they are the cases most likely
 * to make a truthful grant look broken when an autonomous run meets one.
 */
function GitOps({ projectId }: { projectId: string }) {
  const catalogue = useDeclarableGitOps();
  const mine = useProjectGitOps(projectId);
  const declare = useDeclareGitOp();
  const forget = useForgetGitOp();

  const refused =
    (declare.isError && isApiRefusal(declare.error) ? declare.error : null) ??
    (forget.isError && isApiRefusal(forget.error) ? forget.error : null);

  if (catalogue.isPending || mine.isPending) {
    return <p className="text-sm text-text-faint">Reading what the queue may do…</p>;
  }

  if (catalogue.data === undefined || mine.data === undefined) {
    return (
      <ReadFailed
        read={catalogue.data === undefined ? catalogue : mine}
        says="what this project's queue may do on its own"
      />
    );
  }

  const declared = new Set(mine.data);
  const pending = declare.isPending || forget.isPending;
  const built = new Set(catalogue.data.map((operation) => operation.kind));
  const stranded = mine.data.filter((kind) => !built.has(kind)).sort();

  function toggle(kind: string, on: boolean) {
    if (on) declare.mutate({ projectId, opKind: kind });
    else forget.mutate({ projectId, opKind: kind });
  }

  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-col gap-2 rounded-lg border border-border bg-surface p-4">
        <p className="max-w-3xl text-xs text-text-muted">
          Ticking a box never lets the agent run the command. For autonomous runs only, the shared
          queue executes the declared operation while the agent's tool call still comes back denied
          with a ticket id; a person never waited for approval, so these grants do not change
          person-run commands. A matching shell rule <span className="font-mono">deny</span> wins and
          makes the tick do nothing. A grant covers only spellings the queue can build: bare{" "}
          <span className="font-mono">git push</span>, <span className="font-mono">-u</span>,{" "}
          <span className="font-mono">--force</span>, and{" "}
          <span className="font-mono">git branch -D</span> still stop and ask. A{" "}
          <span className="font-mono">rebase</span> requested by a run comes back as a blocked ticket
          because that run holds the branch, and the ticket says which worktree holds it.
        </p>

        <GitOpList
          ops={catalogue.data}
          declared={declared}
          pending={pending}
          onToggle={toggle}
        />
      </div>

      {stranded.length === 0 ? null : (
        <div className="flex flex-col gap-2 rounded-lg border border-border bg-surface p-4">
          <p className="text-xs uppercase tracking-wide text-text-faint">No longer built</p>
          <p className="max-w-3xl text-xs text-text-muted">
            This project's table names git operations this daemon does not build. They grant
            nothing, and the rows remain here so they can be withdrawn.
          </p>
          <ul className="flex flex-col gap-1">
            {stranded.map((kind) => (
              <li
                key={kind}
                aria-label={`git operation ${kind}`}
                className="flex flex-wrap items-baseline gap-2 text-sm"
              >
                <span className="rounded-pill border border-border bg-surface-sunken px-2 py-0.5 font-mono text-xs text-text-muted">
                  {kind}
                </span>
                <button
                  type="button"
                  disabled={pending}
                  onClick={() => toggle(kind, false)}
                  className="ml-auto text-xs text-text-faint underline-offset-2 hover:underline disabled:opacity-40"
                >
                  withdraw
                </button>
              </li>
            ))}
          </ul>
        </div>
      )}

      {refused === null ? null : <RefusalNote refusal={refused} sentences={GIT_OP_SENTENCES} />}
    </div>
  );
}

/**
 * Draw the machine's single git-operation catalogue without inventing GitHub-style halves.
 *
 * `declarable: false` is still handled even though this build's six rows are all true: the route's
 * shape makes the ceiling explicit, and a future build may narrow it. Such a row is a fact rather
 * than a disabled control, while a declaration already stored for it keeps the one legal gesture
 * that narrows authority again.
 */
function GitOpList({
  ops,
  declared,
  pending,
  onToggle,
}: {
  ops: DeclarableGitOp[];
  declared: Set<string>;
  pending: boolean;
  onToggle: (kind: string, on: boolean) => void;
}) {
  if (ops.length === 0) {
    return <p className="text-xs text-text-faint">This daemon builds no queue operations.</p>;
  }

  return (
    <ul className="flex flex-col gap-1">
      {ops.map((operation) => (
        <li
          key={operation.kind}
          aria-label={`git operation ${operation.kind}`}
          className="flex flex-wrap items-baseline gap-2 text-sm"
        >
          {operation.declarable ? (
            <label className="flex items-center gap-1.5">
              <input
                type="checkbox"
                checked={declared.has(operation.kind)}
                disabled={pending}
                onChange={(event) => onToggle(operation.kind, event.target.checked)}
              />
              <span className="font-mono text-xs text-text">{operation.kind}</span>
            </label>
          ) : (
            <>
              <span className="rounded-pill border border-border bg-surface-sunken px-2 py-0.5 font-mono text-xs text-text-muted">
                {operation.kind}
              </span>
              <span className="text-xs text-text-faint">
                {declared.has(operation.kind) ? OUTSIDE_AND_DECLARED : OUTSIDE_THE_CEILING}
              </span>
              {declared.has(operation.kind) ? (
                <button
                  type="button"
                  disabled={pending}
                  onClick={() => onToggle(operation.kind, false)}
                  className="ml-auto text-xs text-text-faint underline-offset-2 hover:underline disabled:opacity-40"
                >
                  withdraw
                </button>
              ) : null}
            </>
          )}
        </li>
      ))}
    </ul>
  );
}

const GIT_OP_SENTENCES: Record<string, string> = {
  kill_switch:
    "the emergency stop is engaged, and granting a queue operation widens what autonomous runs may do. Withdrawing one is never blocked by it.",
  no_such_project: "the núcleo has no project by this name.",
  no_such_op: "this project had not declared that git operation.",
  internal: "the núcleo hit an error of its own writing it down.",
};

/* -------------------------------------- 4. what the worktrees may run -- */

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
   *
   * **Never reached from a write rule, and `RuleList` is where that is enforced rather than here.**
   * The opposite verdict of a `deny Edit …` is an `allow` the route answers 422 to — a page that
   * offered the gesture would be making a request it already knows the answer to, and putting a
   * refusal on screen that says nothing about anything the owner did wrong.
   */
  function flip(rule: ShellRule) {
    const note: Note = rule.note === null ? { erase: true } : { write: rule.note };
    declare.mutate({
      projectId,
      prefix: rule.prefix,
      tool: rule.tool,
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

      {/*
        **Both captions say only what is true of EVERY row beneath them**, which is a smaller claim
        than either used to make, and the shrinking is the point.

        One list holds two kinds of rule now, and a caption is read as a guarantee over all of it.
        "Never runs here, and never written to" sat over a list of command prefixes and promised
        the second half about them — but a write rule is gated on `classifier::WRITE_TOOLS`, which
        is `Edit` and `Write` and nothing else: it does not stop a `Bash` or `PowerShell` line
        redirecting into the same directory, and `NotebookEdit` is outside that list entirely. An
        owner who read the old sentence over `deny rm -rf` came away believing the directory was
        closed to writes, which no rule on this page says.

        So the guarantee is a property of the ROW — the tool is on it, and the sentence beside it
        names what that tool may not do — and the caption is what remains true across the list.
      */}
      <RuleList
        title="Allowed"
        says="A command prefix here runs without stopping to ask, in this project's worktrees."
        rows={allow}
        pending={pending}
        onFlip={flip}
        onForget={(rule) => forget.mutate({ projectId, prefix: rule.prefix, tool: rule.tool })}
      />
      <RuleList
        title="Refused"
        says="Refused here, whatever the compiled lists would have said. A prefix standing alone never runs; a prefix behind a tool is never written to by that tool — a command redirecting into the same path is the command list's business."
        rows={deny}
        pending={pending}
        onFlip={flip}
        onForget={(rule) => forget.mutate({ projectId, prefix: rule.prefix, tool: rule.tool })}
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
          No page copy for `unmatchable_prefix`, `unenforceable_allow` or `unknown_tool`, and the
          omission is the decision. `RefusalNote` prefers a named sentence OVER the daemon's detail,
          so an entry in the table below does not add to those three — it HIDES them. Each of the
          three details names the offending value and then says what would work instead: the prefix
          that could never fire and the `deny` that would be enforced; the tool that cannot be
          allowed and the refusal that can; the tool nobody governs and the two that exist. No
          sentence this page could write would be better, so it writes none, and this comment is
          what stops somebody adding one later.
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

/**
 * PURE: how one row names itself, to a screen reader and to a test.
 *
 * **A prefix stopped being a name the moment two rules could share one.** `deny migrations` and
 * `deny Edit migrations` are two rules a project may hold at once, and both drawn as "rule
 * migrations" is one name given to two rows — a label somebody navigating by voice cannot use to
 * pick between them, and a query that finds whichever came first.
 *
 * So a write rule says the whole claim and not the prefix: it is not a rule *about* `migrations`,
 * it is a rule about `Edit` writing to `migrations`. A command rule keeps the name it has always
 * had, because it is still the only rule of its kind that can carry that prefix.
 */
function ruleLabel(rule: ShellRule): string {
  return rule.tool === null ? `rule ${rule.prefix}` : `rule ${rule.tool} writing to ${rule.prefix}`;
}

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
  /** The whole rule and not its prefix: the DELETE needs the tool to name the row — see {@link useForgetShellRule}. */
  onForget: (rule: ShellRule) => void;
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
              /*
                The tool and the prefix, because that pair is the rule's identity in the núcleo's
                own unique index. Keyed on the prefix alone, a project holding both kinds of rule
                for one name hands React two children with one key — which it resolves by drawing
                one of them. The separator is a NUL because no prefix and no tool can contain one,
                so no pair of rules can collide by spelling their way across it.
              */
              key={`${rule.tool ?? ""}\u0000${rule.prefix}`}
              aria-label={ruleLabel(rule)}
              className="flex flex-wrap items-baseline gap-2 text-sm"
            >
              {/*
                A write rule wears its tool and a command rule does not, and that IS the visual
                grammar — a prefix standing alone is a command, a prefix with a tool in front of it
                is a path. It needs no legend because the row reads as the sentence it means.

                **Three cases and not two, and the third is the one this page must not get wrong.**
                This used to say the sentence "can only be 'may not write to': the route refuses an
                `allow` beside a tool, so there is no other verb a row here can have" — true of the
                ROUTE and false of the TABLE, and this list is served from the table. A tool-carrying
                `allow` can be in it: `post_project_shell_rule` refuses one at the door, but a row
                written before that guard existed, by an out-of-band write, or by a migration, is
                still a row — and `project_policy::declared_shell_rules` serves the table whole on
                purpose, leaving the deciding read (`shell_rules`) to drop it with a `tracing::warn!`
                nobody standing here will ever see.

                Drawn with the fixed phrase, such a row landed in the **Allowed** panel wearing a
                refusal's words: a permission nothing enforces, dressed as a rule in force. It is not
                filtered out either — hiding it would leave the owner unable to find the row they
                would have to withdraw. So it gets the one sentence that is true of it, the negation
                FIRST so a fast read cannot take the affirmative half alone, and the `forget` button
                beside every other row is the way out of it.
              */}
              {rule.tool === null ? null : rule.verdict === "deny" ? (
                <span className="text-xs text-text-muted">
                  <span className="font-mono text-text">{rule.tool}</span> may not write to
                </span>
              ) : (
                <span className="text-xs text-text-muted">
                  nothing enforces this —{" "}
                  <span className="font-mono text-text">{rule.tool}</span> allowed to write to
                </span>
              )}
              {/*
                The FOLDED spelling, which is what is stored and what is enforced. Echoing what
                somebody typed would be showing them a rule the classifier has never heard of. A
                path keeps its case here and a command does not, which is the daemon's doing and
                not this row's — see `foldPathPrefix`.
              */}
              <span className="font-mono text-xs text-text">{rule.prefix}</span>
              {/*
                What to DO about it, and only on the row that needs doing something about. The
                sentence above says the row decides nothing; without this one the owner is left
                holding that fact and no move. Both moves are named because they are different
                intentions — the rule was a mistake, or the rule was meant and was written with the
                wrong verdict — and this page cannot know which.
              */}
              {rule.tool !== null && rule.verdict === "allow" ? (
                <span className="text-xs text-text-faint">
                  a rule about a tool can only refuse; withdraw it, or declare it as a refusal
                </span>
              ) : null}
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
              <span className="ml-auto flex items-baseline gap-2">
                {/*
                  **Not offered on a write rule at all**, and absent rather than disabled. The
                  opposite verdict of a `deny Edit …` is the one `post_project_shell_rule` refuses
                  with `unenforceable_allow`, so the control would be a request the page knows will
                  fail. A disabled button still says "this is a switch, and it is off"; the truth is
                  that a write rule has one verdict and there is no switch — the same reading §5.2
                  takes about an operation outside the ceiling, three sections up.

                  It stays absent on the stranded `allow Edit …` above too, where the opposite
                  verdict WOULD be accepted, and that is deliberate rather than an oversight the
                  new case walked into. "Allow" is not a verdict a write rule has, so a switch on
                  such a row would draw it as one end of a pair — which is the very picture the row
                  now spends a sentence undoing. The move is spelled out beside the prefix instead,
                  where it can say which of the two things the owner might have meant.
                */}
                {rule.tool === null ? (
                  <button
                    type="button"
                    disabled={pending}
                    onClick={() => onFlip(rule)}
                    className="text-xs text-text-faint underline-offset-2 hover:underline disabled:opacity-40"
                  >
                    {rule.verdict === "allow" ? "refuse it instead" : "allow it instead"}
                  </button>
                ) : null}
                <button
                  type="button"
                  disabled={pending}
                  onClick={() => onForget(rule)}
                  className="text-xs text-text-faint underline-offset-2 hover:underline disabled:opacity-40"
                >
                  forget
                </button>
              </span>
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
 * **The identity of a rule is its tool and its FOLDED prefix**, so this form folds before it looks:
 * asking with the typed spelling is how a form offers to create a rule that already exists and then
 * overwrites it without saying so. What will be stored is previewed under the box for the same
 * reason — and WHICH fold is previewed follows the tool, because a path keeps its case and a
 * command does not. Previewing a lower-cased path would be the surprise this preview exists to
 * prevent, told from the other side.
 *
 * **Choosing a tool takes `allow` off the form rather than letting the route refuse it.** A rule
 * about a tool can only deny — `unenforceable_allow`, and the two silent enforcements behind it —
 * so a form that still offered the button would be inviting somebody to a 422 it could have
 * answered itself. The control is removed and the reason put in its place, which is what makes
 * this a fact about write rules rather than a validation somebody trips over.
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
  /** `null` is a rule about a command prefix, which is what this form could only declare before. */
  const [tool, setTool] = useState<string | null>(null);

  // The núcleo folds a path and a command by two different functions, and the difference is the
  // case: `fold_path_prefix` deliberately does not lower-case, because whether a path's case
  // matters is the filesystem's question and it is answered at comparison time.
  const folded = tool === null ? foldPrefix(prefix) : foldPathPrefix(prefix);
  const existing = declaredRule(rows, prefix, tool);
  const ready = folded !== "";

  function submit(verdict: Verdict) {
    declare.mutate(
      {
        projectId,
        prefix,
        tool,
        verdict,
        // Two operations and two members, because the route cannot be told "leave the note alone".
        note: note.trim() === "" ? { erase: true } : { write: note.trim() },
      },
      {
        // Cleared only on success, so a refused declaration leaves what was typed in front of the
        // person who typed it — the rule `Commands` already follows. The TOOL is deliberately not
        // cleared: it is the kind of rule somebody is writing, not the rule, and a project closing
        // three directories to `Edit` should not have to say `Edit` three times.
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
        {/*
          What KIND of rule this is, asked before the prefix because it changes what the prefix
          means: a command to run, or a path to write into. The two the núcleo can govern are
          `classifier::WRITE_TOOLS`, and the route refuses anything else by name — a third option
          here would be a control whose only answer is `unknown_tool`.
        */}
        <select
          aria-label="What this rule is about"
          value={tool ?? ""}
          onChange={(event) => setTool(event.target.value === "" ? null : event.target.value)}
          className="rounded-md border border-border bg-surface-sunken px-2 py-1 text-sm text-text"
        >
          <option value="">a command</option>
          <option value="Edit">Edit writing to a path</option>
          <option value="Write">Write writing to a path</option>
        </select>
        <input
          /*
            The label follows the choice, because with a tool selected this box holds a PATH and
            "Command prefix" would be naming it after the other kind of rule — to a screen reader,
            which has nothing else to go on, and to whoever reads the placeholder.
          */
          aria-label={tool === null ? "Command prefix" : "Path prefix"}
          placeholder={tool === null ? "bash scripts/gates.sh" : "core/migrations"}
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
        {/*
          **Gone when a tool is chosen, and not merely disabled.** A rule about a tool can only
          refuse: the write chain in `classifier::classify` has no allow side to reach, so the route
          answers `unenforceable_allow` and `project_policy::shell_rules` would drop the row anyway.
          Leaving a disabled button would say "this is a switch, and it is off" about a switch that
          does not exist — and leaving it enabled would send a request whose refusal is the page's
          own fault.
        */}
        {tool === null ? (
          <button
            type="button"
            disabled={!ready || declare.isPending}
            onClick={() => submit("allow")}
            className="rounded-md border border-border px-3 py-1.5 text-xs text-text enabled:hover:border-border-strong disabled:opacity-40"
          >
            allow it here
          </button>
        ) : null}
        <button
          type="button"
          disabled={!ready || declare.isPending}
          onClick={() => submit("deny")}
          className="rounded-md border border-border px-3 py-1.5 text-xs text-text enabled:hover:border-border-strong disabled:opacity-40"
        >
          refuse it here
        </button>
        {tool === null ? (
          <span className="text-xs text-text-faint">
            A refusal may take a shape an allow may not — a pipe, a redirection, an{" "}
            <span className="font-mono">-exec</span> — because a refusal answers at the whole line
            and needs no shape the classifier can read.
          </span>
        ) : (
          <span className="text-xs text-text-faint">
            A rule about <span className="font-mono">{tool}</span> can only refuse. There is nothing
            to allow: a write the núcleo does not refuse is already local work it does not stop for,
            so a permission here would widen nothing and would be enforced by nothing. The path is
            read from the project root, and everything under it is refused with it.
          </span>
        )}
      </div>
    </div>
  );
}

/* -------------------------------------------- 5. where the work lands -- */

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
