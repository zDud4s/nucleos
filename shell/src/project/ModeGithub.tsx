// §spec alcada-por-projecto
import { useState, type FormEvent, type ReactNode } from "react";
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
  type Note,
  type ShellRule,
  type Verdict,
} from "../data/project-policy";
import { useKillSwitch } from "../data/system";
import { clock } from "../lib/timeline";
import {
  Button,
  ConfirmButton,
  ConflictNote,
  ErrorNote,
  Field,
  Inset,
  Quiet,
  RefusalNote,
  Section,
  Well,
} from "../ui";

/**
 * "What may this project do without asking?" — the Authority mode.
 *
 * The fifth mode, and it earns the tab by the rule the other four are held to: it has a subject of
 * its own — this project's authority, which is four tables (GitHub operations, git operations,
 * shell rules, landing targets) and not a setting — and a shape of its own, a status line over five
 * sections read top to bottom rather than a grid of panels. §5 of
 * `.ai/specs/2026-09-03-alcada-por-projecto-design.md` and §7 of
 * `.ai/specs/2026-09-06-git-ops-declaraveis-design.md` fix the order and the reason for it: the
 * remote first, because that is what somebody arrives to look at, and the government under it.
 *
 * **The tab was called "GitHub" and only its first section was.** Four of the five sections govern
 * local authority — the queue, the worktrees' shell, where work lands — so the label promised a
 * page someone who does not use GitHub would never open, although it governs their worktrees too.
 * `Workspace.tsx` still answers the old `github` segment.
 *
 * **The order below is a slot map, and it is written down because the page is about to grow.** The
 * status line answers "is everything fine?" first (Product Principle 1). Then the remote. Then the
 * tables, broadest first: GitHub, the queue, the shell, where work lands. The mode and the proposal
 * ceiling (today in State › Settings) and the inspector's "On its own" block (Judge, WIP ceiling,
 * rules) are meant to move here; they are broader than any one table, so they belong between the
 * remote and "GitHub operations without asking", and the status line should count them once they
 * arrive.
 *
 * **Everything below the first section is a live autonomy control and not a configuration edit.**
 * `hooks.rs` reads the shell table per tool call, with no cache and no restart, so a rule declared
 * here binds the very next tool call of every in-flight run of this project. That is what the page
 * says first, and the asymmetry it implies is drawn in the controls rather than argued in prose:
 * **narrowing is one press, widening is two.** Withdrawing, refusing, un-granting all happen at
 * once. Granting an operation, allowing a prefix, flipping a refusal to a permission and
 * withdrawing a refusal all go through `ConfirmButton`, whose armed label says what is about to
 * be widened. The emergency stop refuses the widening writes, so while it is engaged those
 * controls are unavailable and the status line says why, instead of letting each one earn a 423.
 *
 * **Not polled**, like the data layer under it: a declaration changes when a person edits it and at
 * no other time, and a timer here would ask three questions every three seconds to watch a list
 * only this window can change. The kill switch is the exception, and it is already polled for the
 * whole app; this page reads the same cache entry.
 */

export interface ModeGithubProps {
  projectId: string;
}

export function ModeGithub({ projectId }: ModeGithubProps) {
  const kill = useKillSwitch();
  // Only a reading of "engaged" blocks anything. An unanswered stop is not an engaged one, and the
  // daemon refuses the write anyway if it is — the control is a courtesy, never the gate.
  const stopped = kill.data?.engaged === true;

  return (
    <div className="flex flex-col gap-8">
      <Standing projectId={projectId} stopped={stopped} />

      <Section label="The remote">
        <Remote projectId={projectId} />
      </Section>

      <Section label="GitHub operations without asking">
        <AutonomousOps projectId={projectId} stopped={stopped} />
      </Section>

      <Section label="Git operations the queue may perform">
        <GitOps projectId={projectId} stopped={stopped} />
      </Section>

      <Section label="What the worktrees may run">
        <ShellRules projectId={projectId} stopped={stopped} />
      </Section>

      <Section label="Where the work lands">
        <LandTargets projectId={projectId} />
      </Section>
    </div>
  );
}

/* ------------------------------------------------------ the one status line -- */

/**
 * The answer to "is everything fine?", before any section asks a question of its own.
 *
 * **One line of counts, read from the caches the sections below fill.** Every hook here is one a
 * section also calls, with the same key, so this adds readers and never requests: the line and the
 * tables cannot disagree about what is declared. A count whose query has not answered is the em
 * dash, which in this app means "a reading nobody took" — a nought there would be claiming a
 * measurement.
 *
 * **Exceptions take the rest of the block, and only when there is one.** The stop engaged, and CI
 * with a run that did not succeed. A calm project gets the counts and nothing else, which is
 * Principle 2 — the normal recedes.
 */
function Standing({ projectId, stopped }: { projectId: string; stopped: boolean }) {
  const githubCatalogue = useDeclarableGithubOps();
  const githubMine = useProjectGithubOps(projectId);
  const gitCatalogue = useDeclarableGitOps();
  const gitMine = useProjectGitOps(projectId);
  const rules = useProjectShellRules(projectId);
  const landing = useProjectLandTargets(projectId);
  const mapping = useProjectRepo(projectId);
  const repo = mapping.data?.state === "known" ? mapping.data.repo : null;
  const runs = useGithubListing("run_list", repo);

  // Only what is IN FORCE is counted: a declared read this build admits. A declared action is
  // recorded and inert, and a row outside the ceiling is narrowed away — counting either would be
  // the status line promising authority the núcleo does not grant.
  const githubReads =
    githubCatalogue.data === undefined || githubMine.data === undefined
      ? undefined
      : githubCatalogue.data.filter(
          (op) => op.half === "read" && op.declarable && githubMine.data.includes(op.kind),
        ).length;
  const gitGranted =
    gitCatalogue.data === undefined || gitMine.data === undefined
      ? undefined
      : gitCatalogue.data.filter((op) => op.declarable && gitMine.data.includes(op.kind)).length;
  // A tool-carrying `allow` decides nothing (see `RuleList`), so it is not a command allowed.
  const allowed = rules.data?.filter((rule) => rule.verdict === "allow" && rule.tool === null).length;
  const refused = rules.data?.filter((rule) => rule.verdict === "deny").length;
  const ci = runs.data === undefined ? null : ciVerdict(runs.data);

  let lands: ReactNode = undefined;
  if (landing.data !== undefined) {
    const { integration, targets } = landing.data;
    const others = targets.length === 0 ? "" : ` +${targets.length}`;
    lands =
      integration.state === "declared" ? (
        <>
          {integration.branch}
          {others}
        </>
      ) : (
        // Stale, derived or unknown: nothing lands by default with confidence, and the section
        // below says which. The line only refuses to claim a branch.
        <>
          no default{others}
        </>
      );
  }

  return (
    <div className="flex flex-col gap-3" aria-label="Standing" role="group">
      <Quiet says="Changes here reach the very next tool call of every run already working in this project.">
        The núcleo reads these tables at the moment it decides, with no cache and no restart, so this
        is a live control and not a preference. Narrowing takes effect at the first press; anything
        that widens what runs may do on their own asks you to press twice.
      </Quiet>

      <dl className="flex flex-wrap items-baseline gap-x-6 gap-y-2 text-sm">
        <Fact term="GitHub reads" value={githubReads} />
        <Fact term="Git operations" value={gitGranted} />
        <Fact term="Commands allowed" value={allowed} />
        <Fact term="Refusals" value={refused} />
        <Fact term="Lands on" value={lands} />
        {ci === null || ci.listed === 0 ? null : (
          <Fact
            term="CI"
            value={ci.failed === 0 ? "no failures listed" : `${ci.failed} did not succeed`}
            wrong={ci.failed > 0}
          />
        )}
      </dl>

      {stopped ? (
        <ConflictNote>
          The emergency stop is engaged, so nothing on this page can widen what runs do on their own
          until it is released. Withdrawing and refusing still work.
        </ConflictNote>
      ) : null}
    </div>
  );
}

/**
 * One count on the status line.
 *
 * The value is the daemon's, so it is set in mono with tabular figures; the term is the page's, in
 * the body face. `wrong` is the one exception this line can carry, and it wears the Wrong Red
 * triple in full rather than a bare foreground.
 */
function Fact({
  term,
  value,
  wrong = false,
}: {
  term: string;
  value: ReactNode | number | undefined;
  wrong?: boolean;
}) {
  return (
    <div className="flex items-baseline gap-1.5">
      <dt className="text-text-muted">{term}</dt>
      <dd
        className={
          wrong
            ? "rounded-sm border border-tone-danger-border bg-tone-danger-bg px-1.5 font-mono text-xs tabular-nums text-tone-danger-fg"
            : "font-mono text-xs tabular-nums text-text"
        }
      >
        {value === undefined ? "—" : value}
      </dd>
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
 * **Asked once, never polled — and so it says WHEN it was asked.** Each listing is a subprocess and
 * a network call, and a three-second timer would have this window running `gh` for ever to redraw
 * a panel nobody is looking at. The remote does change without anybody here touching it, though —
 * that is what CI is — and a window stays open for hours. So the read's own time sits beside the
 * one control that asks again: a listing from three hours ago shown without a time is currency this
 * page does not have (Principle 4).
 */
function Remote({ projectId }: { projectId: string }) {
  const mapping = useProjectRepo(projectId);
  const found = mapping.data;
  const repo = found?.state === "known" ? found.repo : null;
  const prs = useGithubListing("pr_list", repo);
  const runs = useGithubListing("run_list", repo);

  if (mapping.isPending) {
    return <Reading>Asking which repository this project is…</Reading>;
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
  // The OLDER of the two reads, because the line has to be true of both listings under it. Zero is
  // TanStack's "never answered", which is a refusal or a read still in flight — each says so itself.
  const answeredAt = [prs.dataUpdatedAt, runs.dataUpdatedAt].filter((at) => at > 0);
  const readAt = answeredAt.length === 0 ? null : Math.min(...answeredAt);

  return (
    <div className="flex flex-col gap-4">
      <p className="flex flex-wrap items-baseline gap-x-3 gap-y-1 text-sm text-text-muted">
        <span className="font-mono text-text">{found.repo}</span>
        {/*
          The URL beside the slug, because they answer different questions. The slug is what the
          daemon sends `gh` at; the URL is what somebody checks it against when the slug is not the
          repository they were expecting — which is the one failure a mapping can have that looks
          like success.
        */}
        <span className="font-mono text-xs text-text-muted">{found.remote}</span>
        <span className="ml-auto flex items-baseline gap-3">
          {readAt === null ? null : (
            <span className="font-mono text-xs tabular-nums text-text-muted">
              read {clock(readAt)}
            </span>
          )}
          <Button
            onClick={() => {
              void prs.refetch();
              void runs.refetch();
            }}
            disabled={asking}
          >
            {asking ? "asking GitHub…" : "ask again"}
          </Button>
        </span>
      </p>

      <Listing title="Open pull requests" read={prs} />
      <Listing title="The last CI runs" read={runs} ci />
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
      "Switching a project off clears the root it was pointed at. The row stays on the roster and the folder is forgotten. Put it back into shadow or active with a folder, and this section fills itself in.",
  },
  root_missing: {
    says: "the folder this project points at is not there",
    because:
      "The núcleo has a root recorded for this project and nothing is at it, which is what a checkout somebody moved or deleted looks like. Nothing here is lost: point the project at where the folder is now.",
  },
  not_a_repository: {
    says: "this project's folder is not a git repository",
    because:
      "That is allowed: only active mode insists on a repository, so a project can be watched in a folder git knows nothing about. There is no origin to read, and so nothing on GitHub to show.",
  },
  no_remote: {
    says: "this repository has no origin",
    because:
      "A local-only project is a project, and this is what one looks like from here rather than a fault. `git remote add origin …` in that folder is the whole of what this section is waiting for.",
  },
  not_github: {
    says: "this project's origin is not a GitHub repository",
    because:
      "The daemon reads github.com and nothing else: its gh is pointed at the public host, so a repository anywhere else is one it could not fetch even if this page asked. There is nothing wrong here; this section just has nothing to say about it.",
  },
};

/** The one refusal `GET /projects/{id}/github-repo` makes, in this page's voice. */
const MAPPING_SENTENCES: Record<string, string> = {
  not_found: "the núcleo has no project by this name.",
  internal: "the núcleo hit an error of its own working out which repository this is.",
};

/** The one loading line, so the page does not grow a third size and a fourth wording for it. */
function Reading({ children }: { children: ReactNode }) {
  return <p className="text-sm text-text-muted">{children}</p>;
}

/**
 * One listing, or the reason there is not one.
 *
 * **Four outcomes and none of them is an empty box.** The daemon refused; `gh` ran and failed; `gh`
 * ran, succeeded and printed nothing; `gh` printed a listing. The third is the one worth naming: a
 * repository with no open pull requests and a `gh` that answered with silence look identical in a
 * panel, which is the sentence §5.1 is built out of, so the empty case says out loud that it is an
 * answer rather than an absence.
 *
 * The title is an `h3`: somebody walking this page by headings found only the five sections, and
 * the two listings are what the first of them is for.
 */
function Listing({
  title,
  read,
  ci = false,
}: {
  title: string;
  read: UseQueryResult<ReadOutcome>;
  ci?: boolean;
}) {
  return (
    <Inset>
      <GroupTitle>{title}</GroupTitle>
      <ListingBody read={read} ci={ci} />
    </Inset>
  );
}

function ListingBody({ read, ci }: { read: UseQueryResult<ReadOutcome>; ci: boolean }) {
  if (read.isPending) {
    return <Reading>Asking GitHub…</Reading>;
  }

  if (read.data === undefined) {
    return isApiRefusal(read.error) ? (
      <RefusalNote refusal={read.error} sentences={sentencesFor(read.error)} />
    ) : (
      <p className="text-sm text-text-muted">
        The núcleo did not answer this read at all. That is the daemon and not GitHub, and asking
        again is the whole treatment.
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
        <p className="text-sm text-text-muted">
          gh ran and did not succeed{outcome.exit_code === null ? "" : ` (exit ${outcome.exit_code})`}
          . This is GitHub's answer rather than the núcleo's, and it is printed whole below.
        </p>
        {said === "" ? (
          <p className="text-sm text-text-muted">And it printed nothing at all while failing.</p>
        ) : (
          /*
            A `Well` and not a bare `pre`: this is a raw payload, and the recess is what says the
            text was not written here. `capped` because a failing `gh` can print any amount, and a
            panel whose height is set by the longest thing it has ever printed is one nobody can
            scan past — the cap scrolls instead, in both directions, which is what keeps the
            terminal's own column alignment intact.
          */
          <Well as="pre" capped>
            {said}
          </Well>
        )}
      </div>
    );
  }

  const listed = outcome.stdout.trim();
  if (listed === "") {
    const said = outcome.output_tail.trim();
    return (
      <p className="text-sm text-text-muted">
        gh answered and listed nothing. {said === "" ? "" : `It said: ${said}`}
      </p>
    );
  }

  if (ci) return <CiListing listed={listed} />;

  /*
    The listing itself, in the same recess. It is a terminal table — columns aligned with spaces —
    and any element that reflowed it would turn `gh`'s own formatting into noise, which is why it
    stays a `pre`; `Well` keeps the mono face that claims the machine produced it and scrolls the
    overflow rather than growing the panel.
  */
  return (
    <Well as="pre" capped>
      {listed}
    </Well>
  );
}

/**
 * The CI listing, with the runs that did not succeed standing out of it.
 *
 * **Still `gh`'s text, and still never laid out again.** The argument above against parsing holds:
 * nothing here sorts, drops or re-columns a line. What changed is that a red run looked exactly like
 * a green one, at the one place on the page an exception is most likely (Principle 2). `gh run
 * list` prints one run per line with the status and the conclusion as its first two tab-separated
 * fields, so reading the second word of a `completed` line is enough to mark the line — and a line
 * that does not have that shape is left unmarked rather than guessed at, which is the one answer
 * that cannot be wrong.
 *
 * The lines are joined by the same newlines they came with, so the `pre`'s text is byte for byte
 * what `gh` printed.
 */
function CiListing({ listed }: { listed: string }) {
  const lines = listed.split("\n");
  const verdict = ciVerdictOf(lines);

  return (
    <>
      {verdict.listed === 0 ? null : verdict.failed === 0 ? (
        <p className="text-sm text-text-muted">
          No failed run among the {verdict.listed} listed.
        </p>
      ) : (
        <p className="max-w-[var(--measure-prose)] rounded-md border border-tone-danger-border bg-tone-danger-bg px-3 py-2 text-sm text-tone-danger-fg">
          {verdict.failed} of the {verdict.listed} listed runs did not succeed; they are marked
          below.
        </p>
      )}
      <Well as="pre" capped>
        {lines.map((line, index) => (
          <span key={index}>
            {index === 0 ? null : "\n"}
            {runFailed(line) ? (
              <mark className="rounded-sm bg-tone-danger-bg text-tone-danger-fg ring-1 ring-inset ring-tone-danger-border">
                {line}
              </mark>
            ) : (
              line
            )}
          </span>
        ))}
      </Well>
    </>
  );
}

/** Conclusions `gh` prints for a finished run that did not succeed. Anything else is not flagged. */
const DID_NOT_SUCCEED = new Set([
  "failure",
  "timed_out",
  "cancelled",
  "startup_failure",
  "action_required",
]);

/**
 * PURE: whether one line of `gh run list` is a finished run that did not succeed.
 *
 * Only a line shaped like the CLI's own output answers yes — `completed`, a tab, a conclusion — so a
 * header, a warning or a format this build has never seen is left alone.
 */
function runFailed(line: string): boolean {
  const [status, conclusion] = line.split("\t");
  return status === "completed" && conclusion !== undefined && DID_NOT_SUCCEED.has(conclusion);
}

/** PURE: how many listed lines are runs at all, and how many of those did not succeed. */
function ciVerdictOf(lines: string[]): { listed: number; failed: number } {
  const runs = lines.filter((line) => line.includes("\t"));
  return { listed: runs.length, failed: runs.filter(runFailed).length };
}

/** The same verdict, from a read outcome — `null` when there is no listing to judge. */
function ciVerdict(outcome: ReadOutcome): { listed: number; failed: number } | null {
  if (outcome.exit_code !== 0) return null;
  const listed = outcome.stdout.trim();
  return listed === "" ? null : ciVerdictOf(listed.split("\n"));
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
 * **The 503 covers two different faults and this does not guess which.** A switched-off GitHub
 * setting and a missing `gh` are both 503 — the núcleo keeps them apart in words and not in the
 * status — and sniffing the prose to tell them apart would be a `switch` over sentences, which is
 * the one thing `client.ts` says never to build. So the advice names both places to look and the
 * daemon's sentence above it says which.
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
  return { [refusal.code]: advice === undefined ? said : `${said}. ${advice}` };
}

const ADVICE: Record<number, string> = {
  403: "The token lives in this machine's Credential Manager and the daemon reads it there at every call, so pasting one and asking again is all this needs.",
  503: "Either GitHub is switched off for this machine (enabled in .ai/github.yaml, on the System page) or gh is not installed where the daemon can find it. The sentence before this says which; fix it, then restart the daemon.",
  504: "GitHub or the network took longer than the núcleo waits. Asking again should settle it.",
};

/**
 * Whether GitHub is unreachable from this machine at all, read off the remote's own listing.
 *
 * **The status line's missing half, and the reason it is a 503 and nothing finer.** The daemon has
 * no route that says whether GitHub is switched on for this machine, and the Reads grants below are
 * moot while it is off — `GithubRuntime::policy_for_project` returns `Policy::empty()` whatever a
 * project declared. The remote's listing is the one read that finds out, and it answers 503 both
 * for a switched-off setting and for a missing `gh`. Either way a granted read has nothing to run
 * against, so the Reads group can say so before somebody grants one, without guessing which.
 */
function useGithubUnreachable(projectId: string): boolean {
  const mapping = useProjectRepo(projectId);
  const repo = mapping.data?.state === "known" ? mapping.data.repo : null;
  const prs = useGithubListing("pr_list", repo);
  return isApiRefusal(prs.error) && prs.error.status === 503;
}

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
      failing rather than the connection. Nothing here is lost: the tables are the núcleo's, and it
      is still enforcing them; this is only the picture of them. Reopening the page asks again.
    </Quiet>
  );
}

const NO_SUCH_PROJECT = "the núcleo has no project by this name.";

/**
 * A group's name inside a section, as a heading.
 *
 * These were `<p>`s, so a screen reader walking by headings found the five sections and none of
 * the groups inside them — "Reads", "Refused", "Add a rule" — which are what a person is looking
 * for once they are in the right section. The label rank (11px, 500, tracked, uppercase) in Muted,
 * one rank under the section title so the two do not read as siblings.
 */
function GroupTitle({ children }: { children: ReactNode }) {
  return (
    <h3 className="text-xs font-medium uppercase tracking-wide text-text-muted">{children}</h3>
  );
}

/* ----------------------------------------------------------- row writes -- */

/** What the last write on one row did, and when — the row's own receipt. */
interface Stamp {
  said: string;
  at: number;
}

/**
 * The writes a section makes, tracked per ROW rather than per section.
 *
 * **Why not the mutation's own `isPending` and `error`.** Those describe the hook, not the row: one
 * pending grant used to disable every checkbox in the section, and a refusal landed at the foot of
 * the section, below six rows and saying nothing about which of them it was about. A second write
 * started while the first was in flight also replaced the first's state, so its outcome was lost.
 * `mutateAsync` settles once per call, so each row can keep its own pending flag, its own receipt
 * ("granted · 14:02") and its own refusal.
 *
 * The receipt is this window's memory and not the daemon's: it says what THIS page did and when,
 * and it is gone on reload, where the table itself is the only truth.
 */
function useRowWrites() {
  const [pending, setPending] = useState<ReadonlySet<string>>(new Set());
  const [stamps, setStamps] = useState<ReadonlyMap<string, Stamp>>(new Map());
  const [failed, setFailed] = useState<{ row: string; error: unknown } | null>(null);

  function run(row: string, said: string, write: () => Promise<unknown>) {
    setPending((now) => new Set(now).add(row));
    setFailed(null);
    write()
      .then(() => setStamps((now) => new Map(now).set(row, { said, at: Date.now() })))
      .catch((error: unknown) => setFailed({ row, error }))
      .finally(() =>
        setPending((now) => {
          const next = new Set(now);
          next.delete(row);
          return next;
        }),
      );
  }

  return {
    run,
    isPending: (row: string) => pending.has(row),
    stamp: (row: string) => stamps.get(row),
    failure: (row: string) => (failed?.row === row ? failed.error : null),
    /** The last receipt for a row that is no longer on screen — a withdrawn rule has no row to wear it. */
    gone: (onScreen: ReadonlySet<string>) => {
      let last: [string, Stamp] | null = null;
      for (const entry of stamps) {
        if (!onScreen.has(entry[0]) && (last === null || entry[1].at > last[1].at)) last = entry;
      }
      return last;
    },
  };
}

/** A row's receipt, in the machine's face because the time is the machine's. */
function Receipt({ stamp }: { stamp: Stamp | undefined }) {
  if (stamp === undefined) return null;
  return (
    <span className="font-mono text-xs tabular-nums text-text-muted">
      {stamp.said} · {clock(stamp.at)}
    </span>
  );
}

/**
 * What went wrong with ONE row's write, drawn inside that row.
 *
 * A refusal in the daemon's words, with the page's sentences; anything else is a write that did
 * not come back at all, which is an error and says so.
 */
function RowFailure({ error, sentences }: { error: unknown; sentences: Record<string, string> }) {
  if (error === null) return null;
  return (
    <div className="basis-full">
      {isApiRefusal(error) ? (
        <RefusalNote refusal={error} sentences={sentences} />
      ) : (
        <ErrorNote>The núcleo did not answer this write. Nothing on screen changed.</ErrorNote>
      )}
    </div>
  );
}

/* ------------------------------------------- 2. GitHub operations without asking -- */

/**
 * What each operation DOES, in words, beside the id the daemon knows it by.
 *
 * `pr_list` is a name for the machine; "list open pull requests" is the sentence somebody decides
 * on. The id stays on the row, in mono, because it is what a refusal quotes and what `.ai/github.yaml`
 * is written in. A kind this map does not know is drawn by its id alone — a new operation must not
 * wait for this table to be granted.
 */
const GITHUB_OP_LABEL: Record<string, string> = {
  pr_list: "list open pull requests",
  run_list: "list recent CI runs",
  run_status: "read one CI run's status",
  run_logs: "read a CI run's logs",
  workflow_list: "list the workflows",
  pr_view: "read a pull request",
  pr_diff: "read a pull request's diff",
  pr_thread: "read a pull request's comments",
  issue_view: "read an issue",
  checks_for_ref: "read the checks on a commit",
  workflow_run: "start a workflow",
  run_rerun: "re-run a CI run",
  pr_create: "open a pull request",
  pr_comment: "comment on a pull request",
  issue_close: "close an issue",
  api_read: "call the GitHub API directly",
};

/**
 * The single list of GitHub operations, and the ceiling drawn around it.
 *
 * **What is outside the ceiling is a fact and never a control.** `Settings.tsx` already applies this
 * rule to action classes — its chips are `span`s, and its header says why: *"nothing in the núcleo
 * lets a person grant a class by hand and a chip that looked clickable would be a lie about who
 * decides"*. The same reading holds here, and the design says it in the same words: there is no box
 * to switch `api_read` on because there is no way to switch it on, and `declarable: false` is a fact
 * about the build that no route, no file and no owner can change. So an undeclarable operation gets
 * no control and nothing that can be pressed — only its name and the reason it is out of reach.
 *
 * **The two halves are not drawn as one list**, because they are not in force in the same way. A
 * declared READ is consulted when an agent types `gh` in a worktree. A declared ACTION is recorded
 * and inert — the route stores it and answers the same 204, and a later step wires it. Telling an
 * owner their declared action is in force would be describing a step that has not landed.
 */
function AutonomousOps({ projectId, stopped }: { projectId: string; stopped: boolean }) {
  const catalogue = useDeclarableGithubOps();
  const mine = useProjectGithubOps(projectId);
  const declare = useDeclareGithubOp();
  const forget = useForgetGithubOp();
  const writes = useRowWrites();
  const unreachable = useGithubUnreachable(projectId);

  if (catalogue.isPending || mine.isPending) {
    return <Reading>Reading what this project may do…</Reading>;
  }

  if (catalogue.data === undefined || mine.data === undefined) {
    // Whichever of the two refused. The catalogue first, because a page that cannot say what MAY be
    // declared cannot draw this section at all, while a missing `mine` only costs the state.
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
  // Declared kinds this build has no operation for. `try_github_ops` serves the table RAW — no
  // narrowing, deliberately — so these arrive, and rendering only from the catalogue dropped them.
  const built = new Set(catalogue.data.map((op) => op.kind));
  const stranded = mine.data.filter((kind) => !built.has(kind)).sort();

  const rowProps = {
    declared,
    stopped,
    writes,
    named: "operation",
    labels: GITHUB_OP_LABEL,
    sentences: OP_SENTENCES,
    onGrant: (kind: string) =>
      writes.run(kind, "granted", () => declare.mutateAsync({ projectId, opKind: kind })),
    onWithdraw: (kind: string) =>
      writes.run(kind, "withdrawn", () => forget.mutateAsync({ projectId, opKind: kind })),
  };

  return (
    <div className="flex flex-col gap-4">
      <Quiet says="Each grant lets this project's runs do that on GitHub without stopping to ask you.">
        A granted read is used when an agent types <span className="font-mono">gh …</span> in one of
        this project's worktrees, and only while GitHub is switched on for this machine:{" "}
        <span className="font-mono">enabled: false</span> in{" "}
        <span className="font-mono">.ai/github.yaml</span> (System › Settings) makes the núcleo
        ignore every project's grants, and nothing on this page overrides that. A granted action is
        stored and does nothing yet. An operation this build does not allow is listed so you know it
        exists; nothing on this machine can grant it.
      </Quiet>

      <Inset>
        <GroupTitle>Reads</GroupTitle>
        <Caption>
          Used the next time an agent types gh, while GitHub is switched on for this machine.
        </Caption>
        {unreachable ? (
          /*
            Said BEFORE anybody grants something, which is the difference between this and the
            refusal the remote section already shows: that one explains a listing, this one says a
            tick here is moot. See `useGithubUnreachable` for why a 503 is all it can know.
          */
          <ConflictNote>
            GitHub is not answering on this machine right now (see The remote), so a read granted
            here has nothing to run against until it is.
          </ConflictNote>
        ) : null}
        <OpList ops={reads} empty="This daemon builds no GitHub reads." {...rowProps} />
      </Inset>

      <Inset>
        <GroupTitle>Actions</GroupTitle>
        <Caption>
          Recorded and inert for now: the GitHub tool still asks you first, and wiring this list to
          it is a later step.
        </Caption>
        <OpList ops={actions} empty="This daemon builds no GitHub actions." {...rowProps} />
      </Inset>

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
        <Inset>
          <GroupTitle>No longer built</GroupTitle>
          <Caption>
            This project's table names operations this daemon no longer builds. They decide
            nothing, and you can withdraw them.
          </Caption>
          <OpList
            ops={stranded.map((kind) => ({ kind, declarable: false }))}
            empty=""
            stranded
            {...rowProps}
          />
        </Inset>
      )}
    </div>
  );
}

/**
 * The stop's sentences for the two operation tables.
 *
 * The widening controls are unavailable while the stop is engaged, so `kill_switch` is reached only
 * when the stop engaged between the page's last look and the press — which is exactly when the
 * sentence matters.
 */
const OP_SENTENCES: Record<string, string> = {
  kill_switch:
    "the emergency stop is engaged, and granting an operation widens what runs on its own. Withdrawing one is never blocked by it.",
  no_such_project: "the núcleo has no project by this name.",
  no_such_op: "this project had not granted that one.",
  internal: "the núcleo hit an error of its own writing it down.",
};

const GIT_OP_SENTENCES: Record<string, string> = {
  kill_switch:
    "the emergency stop is engaged, and granting a queue operation widens what autonomous runs may do. Withdrawing one is never blocked by it.",
  no_such_project: "the núcleo has no project by this name.",
  no_such_op: "this project had not granted that git operation.",
  internal: "the núcleo hit an error of its own writing it down.",
};

/** One sentence under a group's heading, capped at the note measure. */
function Caption({ children }: { children: ReactNode }) {
  return <p className="max-w-[var(--measure-prose)] text-xs text-text-muted">{children}</p>;
}

/**
 * One operation table — GitHub's halves and the queue's single list are drawn by the same rows.
 *
 * **Narrowing is one press and widening is two, and that asymmetry is the whole control.** These
 * were checkboxes: a tick widened what every in-flight run of the project may do, with the same
 * weight as an untick, and the page then spent a paragraph saying it was serious. Now the state is
 * a word ("without asking" / "asks first"), withdrawing is a plain button, and granting is a
 * `ConfirmButton` whose armed label says what is about to be widened.
 *
 * `declarable: false` is still handled for the queue's list even though this build's six rows are
 * all true: the route's shape makes the ceiling explicit, and a future build may narrow it. Such a
 * row is a fact rather than a disabled control, while a declaration already stored for it keeps the
 * one legal gesture that narrows authority again.
 */
function OpList({
  ops,
  empty,
  stranded = false,
  declared,
  stopped,
  writes,
  named,
  labels,
  sentences,
  onGrant,
  onWithdraw,
}: {
  ops: { kind: string; declarable: boolean }[];
  empty: string;
  /** Rows the build no longer has: the ceiling caption would be the wrong fact about them. */
  stranded?: boolean;
  declared: Set<string>;
  stopped: boolean;
  writes: ReturnType<typeof useRowWrites>;
  /** What a row calls itself to a screen reader: `operation pr_list`, `git operation push`. */
  named: string;
  labels: Record<string, string>;
  sentences: Record<string, string>;
  onGrant: (kind: string) => void;
  onWithdraw: (kind: string) => void;
}) {
  if (ops.length === 0) {
    return <Quiet says={empty} />;
  }

  return (
    <ul className="flex flex-col gap-1">
      {ops.map((op) => {
        const has = declared.has(op.kind);
        const busy = writes.isPending(op.kind);
        const label = labels[op.kind];
        return (
          <li
            key={op.kind}
            aria-label={`${named} ${op.kind}`}
            className="flex min-h-8 flex-wrap items-baseline gap-x-3 gap-y-1 text-sm"
          >
            {label === undefined ? null : <span className="text-text">{label}</span>}
            {/* The id in the machine's face and nothing more: not a pill, which the system keeps
                for badges, and never the only name the row has when a human one exists. */}
            <span className={label === undefined ? "font-mono text-xs text-text" : "font-mono text-xs text-text-muted"}>
              {op.kind}
            </span>

            {op.declarable ? (
              <span className="ml-auto flex items-baseline gap-3">
                <Receipt stamp={writes.stamp(op.kind)} />
                {has ? (
                  <>
                    <span className="text-xs font-medium text-text">without asking</span>
                    <Button variant="quiet" disabled={busy} onClick={() => onWithdraw(op.kind)}>
                      withdraw
                    </Button>
                  </>
                ) : (
                  <>
                    <span className="text-xs text-text-muted">asks first</span>
                    <ConfirmButton
                      variant="quiet"
                      label="grant"
                      confirmLabel="grant without asking"
                      subject={op.kind}
                      disabled={busy}
                      unavailable={stopped}
                      title={stopped ? "The emergency stop is engaged: nothing may be granted until it is released." : undefined}
                      onConfirm={() => onGrant(op.kind)}
                    />
                  </>
                )}
              </span>
            ) : (
              <>
                {/*
                  A `span` and never a control — the `Settings` chip's argument, applied to an
                  operation instead of to a class. This build does not admit it, so there is nothing
                  for a control to be wired to, and one that refused would be a lie about who
                  decides.
                */}
                {stranded ? null : (
                  <span className="text-xs text-text-muted">
                    {has ? OUTSIDE_AND_DECLARED : OUTSIDE_THE_CEILING}
                  </span>
                )}
                {/*
                  The one legal gesture, and only when there is a row to remove.
                  `delete_project_github_op` has no declarability check, and says why: *"withdrawing
                  narrows, and an operation stored before the ceilings moved still has to be
                  removable"*. `GET .../github-ops` serves the raw table for the same reason, so a
                  row outside the ceilings arrives here on purpose.
                */}
                {has ? (
                  <span className="ml-auto flex items-baseline gap-3">
                    <Receipt stamp={writes.stamp(op.kind)} />
                    <Button variant="quiet" disabled={busy} onClick={() => onWithdraw(op.kind)}>
                      withdraw
                    </Button>
                  </span>
                ) : null}
              </>
            )}

            <RowFailure error={writes.failure(op.kind)} sentences={sentences} />
          </li>
        );
      })}
    </ul>
  );
}

/**
 * The caption for an operation the build does not admit. "Compiled ceiling" was the núcleo's word
 * for it; what somebody deciding needs is the consequence.
 */
const OUTSIDE_THE_CEILING = "not allowed by this build, so nothing on this machine can grant it";

/**
 * The row that used to be a contradiction: stored, and captioned as though it were not.
 *
 * `Policy::for_project` narrows it away, so the AUTHORITY is right and this operation does not run —
 * but the row is in the project's table, the owner cannot see that from the old caption, and the
 * withdraw is the only thing that makes the table agree with the page again.
 */
const OUTSIDE_AND_DECLARED = "granted here, but this build does not allow it, so it does not run";

/* ------------------------------------------- 3. git operations the queue may perform -- */

/** What each queue operation does, beside the id the queue knows it by. See {@link GITHUB_OP_LABEL}. */
const GIT_OP_LABEL: Record<string, string> = {
  merge: "merge a branch",
  push: "push to the remote",
  tag: "create a tag",
  fetch: "fetch from the remote",
  rebase: "rebase a branch",
  "branch-delete": "delete a branch",
};

/**
 * The git operations an autonomous run may hand to the shared queue as already consented.
 *
 * **One list and no GitHub halves.** Every row is a write the queue performs in the same way; a
 * `half` would suggest the live-read versus inert-action distinction that belongs only to GitHub.
 * The project list is still served raw, so a declaration this build no longer constructs is drawn
 * below the catalogue and remains withdrawable instead of disappearing from its owner's view.
 *
 * **The caveats are one press away, not gone.** A grant changes who the queue waits for, not who
 * runs the command, and only after both parsing and shell-rule precedence have admitted that path.
 * The spellings and the rebase case are the ones most likely to make a truthful grant look broken
 * when an autonomous run meets one, so they are behind "why?" on the section's one sentence rather
 * than in a paragraph every opening has to read past.
 */
function GitOps({ projectId, stopped }: { projectId: string; stopped: boolean }) {
  const catalogue = useDeclarableGitOps();
  const mine = useProjectGitOps(projectId);
  const declare = useDeclareGitOp();
  const forget = useForgetGitOp();
  const writes = useRowWrites();

  if (catalogue.isPending || mine.isPending) {
    return <Reading>Reading what the queue may do…</Reading>;
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
  const built = new Set(catalogue.data.map((operation) => operation.kind));
  const stranded = mine.data.filter((kind) => !built.has(kind)).sort();

  const rowProps = {
    declared,
    stopped,
    writes,
    named: "git operation",
    labels: GIT_OP_LABEL,
    sentences: GIT_OP_SENTENCES,
    onGrant: (kind: string) =>
      writes.run(kind, "granted", () => declare.mutateAsync({ projectId, opKind: kind })),
    onWithdraw: (kind: string) =>
      writes.run(kind, "withdrawn", () => forget.mutateAsync({ projectId, opKind: kind })),
  };

  return (
    <div className="flex flex-col gap-4">
      <Quiet says="A granted operation is carried out by the queue for an autonomous run, without waiting for you.">
        The agent's own command still comes back denied, with a ticket id: the queue runs it
        instead. A person running a command never waited for approval, so these grants change
        nothing for them. A matching refusal under "What the worktrees may run" wins and makes the
        grant do nothing. Only spellings the queue can build are covered: bare{" "}
        <span className="font-mono">git push</span>, <span className="font-mono">-u</span>,{" "}
        <span className="font-mono">--force</span> and <span className="font-mono">git branch -D</span>{" "}
        still stop and ask. A <span className="font-mono">rebase</span> a run asks for comes back as a
        blocked ticket, because that run holds the branch, and the ticket says which worktree holds
        it.
      </Quiet>

      <Inset>
        <OpList ops={catalogue.data} empty="This daemon builds no queue operations." {...rowProps} />
      </Inset>

      {stranded.length === 0 ? null : (
        <Inset>
          <GroupTitle>No longer built</GroupTitle>
          <Caption>
            This project's table names git operations this daemon no longer builds. They grant
            nothing, and you can withdraw them.
          </Caption>
          <OpList
            ops={stranded.map((kind) => ({ kind, declarable: false }))}
            empty=""
            stranded
            {...rowProps}
          />
        </Inset>
      )}
    </div>
  );
}

/* -------------------------------------- 4. what the worktrees may run -- */

/**
 * The two lists, `allow` and `deny`, with the rule that orders them written on the page.
 *
 * §5.3 asks for the precedence *«escrita na página e não só no código»*, and it is not decoration:
 * the two lists are not mirror images, and somebody reading them as two ends of one switch will
 * write an `allow` expecting it to lift a refusal. So the rule is the section's one visible
 * sentence, and its worked example is behind "why?".
 *
 * **One row per rule, grouped here rather than served grouped.** `useProjectShellRules` answers
 * `ShellRule[]` — prefix, verdict, note, and the day it was first written down — because the note is
 * the column the whole table exists for and a pair of prefix lists could not carry one.
 */
function ShellRules({ projectId, stopped }: { projectId: string; stopped: boolean }) {
  const rules = useProjectShellRules(projectId);
  const declare = useDeclareShellRule();
  const forget = useForgetShellRule();
  const writes = useRowWrites();

  if (rules.isPending) {
    return <Reading>Reading this project's rules…</Reading>;
  }

  if (rules.data === undefined) {
    return <ReadFailed read={rules} says="what this project's worktrees may run" />;
  }

  const rows = rules.data;
  const allow = rows.filter((rule) => rule.verdict === "allow");
  const deny = rows.filter((rule) => rule.verdict === "deny");

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
    const verdict: Verdict = rule.verdict === "allow" ? "deny" : "allow";
    writes.run(ruleKey(rule), verdict === "allow" ? "allowed" : "refused", () =>
      declare.mutateAsync({ projectId, prefix: rule.prefix, tool: rule.tool, verdict, note }),
    );
  }

  function withdraw(rule: ShellRule) {
    writes.run(ruleKey(rule), "withdrawn", () =>
      forget.mutateAsync({ projectId, prefix: rule.prefix, tool: rule.tool }),
    );
  }

  const onScreen = new Set(rows.map(ruleKey));
  const gone = writes.gone(onScreen);

  return (
    <div className="flex flex-col gap-4">
      {/*
        §4.2's ordering, in the words somebody editing these lists needs. The visible sentence is the
        rule; the worked example behind "why?" is the half nobody guesses, and it stays one press
        away rather than one paragraph in the way of every opening.
      */}
      <Quiet says="A refusal here beats a permission here, and beats one compiled into the núcleo. A permission never lifts a refusal.">
        A permission only widens what the núcleo would otherwise have <em>asked</em> about. So{" "}
        <span className="font-mono">rm</span> allowed here leaves{" "}
        <span className="font-mono">rm -rf /</span> refused, and a line carrying{" "}
        <span className="font-mono">$( )</span> stays refused whatever these lists say. A project with
        no rules at all classifies exactly as it did before there were any.
      </Quiet>

      {/*
        **Both captions say only what is true of EVERY row beneath them**, which is a smaller claim
        than either used to make, and the shrinking is the point.

        One list holds two kinds of rule now, and a caption is read as a guarantee over all of it.
        "Never runs here, and never written to" sat over a list of command prefixes and promised
        the second half about them — but a write rule is gated on `classifier::WRITE_TOOLS`, and
        that list holds the file-writing tools and nothing else: it does not stop a `Bash` or
        `PowerShell` line redirecting into the same directory. An owner who read the old sentence
        over `deny rm -rf` came away believing the directory was closed to writes, which no rule
        on this page says.

        So the guarantee is a property of the ROW — the tool is on it, and the sentence beside it
        names what that tool may not do — and the caption is what remains true across the list.
      */}
      <RuleList
        title="Allowed"
        says="A command prefix here runs without stopping to ask, in this project's worktrees."
        rows={allow}
        stopped={stopped}
        writes={writes}
        onFlip={flip}
        onWithdraw={withdraw}
      />
      <RuleList
        title="Refused"
        says="A prefix standing alone never runs here; a prefix behind a tool is never written to by that tool. A command redirecting into the same path is the command list's business."
        rows={deny}
        stopped={stopped}
        writes={writes}
        onFlip={flip}
        onWithdraw={withdraw}
      />

      {gone === null ? null : (
        <p className="text-xs text-text-muted">
          <span className="font-mono text-text">{gone[0].split("\u0000").filter(Boolean).join(" ")}</span>{" "}
          <Receipt stamp={gone[1]} />
        </p>
      )}

      {/*
        The one mutation, handed down rather than made again inside the form. `useDeclareShellRule`
        is one hook because the núcleo has one operation — the identity of a rule is its folded
        prefix, so declaring and re-verdicting are the same POST.
      */}
      <DeclareRule projectId={projectId} rows={rows} declare={declare} stopped={stopped} />
    </div>
  );
}

/*
  No page copy for `unmatchable_prefix`, `unenforceable_allow` or `unknown_tool`, and the omission is
  the decision. `RefusalNote` prefers a named sentence OVER the daemon's detail, so an entry in the
  table below does not add to those three — it HIDES them. Each of the three details names the
  offending value and then says what would work instead: the prefix that could never fire and the
  `deny` that would be enforced; the tool that cannot be allowed and the refusal that can; the tool
  nobody governs and the two that exist. No sentence this page could write would be better, so it
  writes none, and this comment is what stops somebody adding one later.
*/
const RULE_SENTENCES: Record<string, string> = {
  /*
    The stop refuses an `allow` and never a `deny`, and the second half is the part worth writing:
    the daemon sends this refusal with no prose at all, and the shared floor's sentence would leave
    somebody believing the stop has closed the whole page.
  */
  kill_switch:
    "the emergency stop is engaged, so nothing here may widen what runs on its own. Declaring the same prefix as a refusal is not blocked: the stop never stands in the way of narrowing.",
  empty_prefix: "a rule has to name a prefix.",
  no_such_project: "the núcleo has no project by this name.",
  no_such_rule: "no rule of that name was declared here.",
  internal: "the núcleo hit an error of its own writing the rule.",
};

/**
 * PURE: the rule's identity, as the núcleo's unique index has it — the tool and the prefix.
 *
 * The separator is a NUL because no prefix and no tool can contain one, so no pair of rules can
 * collide by spelling their way across it.
 */
function ruleKey(rule: ShellRule): string {
  return `${rule.tool ?? ""}\u0000${rule.prefix}`;
}

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
  stopped,
  writes,
  onFlip,
  onWithdraw,
}: {
  title: string;
  says: string;
  rows: ShellRule[];
  stopped: boolean;
  writes: ReturnType<typeof useRowWrites>;
  onFlip: (rule: ShellRule) => void;
  /** The whole rule and not its prefix: the DELETE needs the tool to name the row — see {@link useForgetShellRule}. */
  onWithdraw: (rule: ShellRule) => void;
}) {
  return (
    <Inset>
      <GroupTitle>{title}</GroupTitle>
      <Caption>{says}</Caption>
      {rows.length === 0 ? (
        <Quiet says="None declared." />
      ) : (
        <ul className="flex flex-col gap-1">
          {rows.map((rule) => {
            const key = ruleKey(rule);
            const busy = writes.isPending(key);
            return (
              <li
                /*
                  The tool and the prefix, because that pair is the rule's identity in the núcleo's
                  own unique index. Keyed on the prefix alone, a project holding both kinds of rule
                  for one name hands React two children with one key — which it resolves by drawing
                  one of them.
                */
                key={key}
                aria-label={ruleLabel(rule)}
                className="flex min-h-8 flex-wrap items-baseline gap-x-3 gap-y-1 text-sm"
              >
                {/*
                  A write rule wears its tool and a command rule does not, and that IS the visual
                  grammar — a prefix standing alone is a command, a prefix with a tool in front of it
                  is a path. It needs no legend because the row reads as the sentence it means.

                  **Three cases and not two, and the third is the one this page must not get wrong.**
                  A tool-carrying `allow` can be in the table: `post_project_shell_rule` refuses one
                  at the door, but a row written before that guard existed, by an out-of-band write,
                  or by a migration, is still a row — and `project_policy::declared_shell_rules`
                  serves the table whole on purpose, leaving the deciding read (`shell_rules`) to
                  drop it with a `tracing::warn!` nobody standing here will ever see.

                  Drawn with the fixed phrase, such a row landed in the **Allowed** panel wearing a
                  refusal's words: a permission nothing enforces, dressed as a rule in force. It is
                  not filtered out either — hiding it would leave the owner unable to find the row
                  they would have to withdraw. So it gets the one sentence that is true of it, the
                  negation FIRST so a fast read cannot take the affirmative half alone.
                */}
                {rule.tool === null ? null : rule.verdict === "deny" ? (
                  <span className="text-xs text-text-muted">
                    <span className="font-mono text-text">{rule.tool}</span> may not write to
                  </span>
                ) : (
                  <span className="text-xs text-text-muted">
                    nothing enforces this:{" "}
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
                  What to DO about it, and only on the row that needs doing something about. Both
                  moves are named because they are different intentions — the rule was a mistake, or
                  the rule was meant and was written with the wrong verdict — and this page cannot
                  know which.
                */}
                {rule.tool !== null && rule.verdict === "allow" ? (
                  <span className="text-xs text-text-muted">
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
                <span className="ml-auto flex items-baseline gap-3">
                  <Receipt stamp={writes.stamp(key)} />
                  {/*
                    **Not offered on a write rule at all**, and absent rather than disabled. The
                    opposite verdict of a `deny Edit …` is the one `post_project_shell_rule` refuses
                    with `unenforceable_allow`, so the control would be a request the page knows will
                    fail. It stays absent on the stranded `allow Edit …` above too: "allow" is not a
                    verdict a write rule has, so a switch on such a row would draw it as one end of a
                    pair — which is the very picture the row spends a sentence undoing.

                    **Refusing is one press and allowing is two.** Flipping a refusal to a permission
                    widens what every run already working here may do, so it arms first.
                  */}
                  {rule.tool !== null ? null : rule.verdict === "allow" ? (
                    <Button variant="quiet" disabled={busy} onClick={() => onFlip(rule)}>
                      refuse it instead
                    </Button>
                  ) : (
                    <ConfirmButton
                      variant="quiet"
                      label="allow it instead"
                      confirmLabel="allow without asking"
                      subject={rule.prefix}
                      disabled={busy}
                      unavailable={stopped}
                      title={stopped ? "The emergency stop is engaged: nothing may be allowed until it is released." : undefined}
                      onConfirm={() => onFlip(rule)}
                    />
                  )}
                  {/*
                    Withdrawing a PERMISSION narrows and is immediate. Withdrawing a REFUSAL widens —
                    whatever it stopped, runs may now be asked about or do — and it takes the rule's
                    justification with it, so it arms and says what it loses.
                  */}
                  {rule.verdict === "allow" ? (
                    <Button variant="quiet" disabled={busy} onClick={() => onWithdraw(rule)}>
                      withdraw
                    </Button>
                  ) : (
                    <ConfirmButton
                      variant="quiet"
                      label="withdraw"
                      confirmLabel={
                        rule.note === null ? "stop refusing this" : "stop refusing this, and drop its justification"
                      }
                      subject={rule.prefix}
                      disabled={busy}
                      onConfirm={() => onWithdraw(rule)}
                    />
                  )}
                </span>
                <RowFailure error={writes.failure(key)} sentences={RULE_SENTENCES} />
              </li>
            );
          })}
        </ul>
      )}
    </Inset>
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
 * command does not.
 *
 * **Choosing a tool takes `allow` off the form rather than letting the route refuse it.** A rule
 * about a tool can only deny — `unenforceable_allow`, and the two silent enforcements behind it —
 * so a form that still offered the button would be inviting somebody to a 422 it could have
 * answered itself. The control is removed and the reason put in its place.
 *
 * **A `<form>`, and Enter submits the REFUSAL.** Enter did nothing before, so the only way to
 * declare was the mouse. The key now does the one thing that is safe to do by reflex: refusing
 * narrows, and is undone by one flip. Allowing is never the default action of a keystroke — it arms
 * and asks for a second press, like every other widening on this page.
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
  stopped,
}: {
  projectId: string;
  rows: ShellRule[];
  declare: ReturnType<typeof useDeclareShellRule>;
  stopped: boolean;
}) {
  const [prefix, setPrefix] = useState("");
  const [note, setNote] = useState("");
  /** `null` is a rule about a command prefix, which is what this form could only declare before. */
  const [tool, setTool] = useState<string | null>(null);
  const [sending, setSending] = useState(false);
  const [failed, setFailed] = useState<unknown>(null);

  // The núcleo folds a path and a command by two different functions, and the difference is the
  // case: `fold_path_prefix` deliberately does not lower-case, because whether a path's case
  // matters is the filesystem's question and it is answered at comparison time.
  const folded = tool === null ? foldPrefix(prefix) : foldPathPrefix(prefix);
  const existing = declaredRule(rows, prefix, tool);
  const ready = folded !== "" && !sending;

  function submit(verdict: Verdict) {
    if (folded === "") return;
    setSending(true);
    setFailed(null);
    declare
      .mutateAsync({
        projectId,
        prefix,
        tool,
        verdict,
        // Two operations and two members, because the route cannot be told "leave the note alone".
        note: note.trim() === "" ? { erase: true } : { write: note.trim() },
      })
      .then(() => {
        // Cleared only on success, so a refused declaration leaves what was typed in front of the
        // person who typed it — the rule `Commands` already follows. The TOOL is deliberately not
        // cleared: it is the kind of rule somebody is writing, not the rule, and a project closing
        // three directories to `Edit` should not have to say `Edit` three times.
        setPrefix("");
        setNote("");
      })
      .catch((error: unknown) => setFailed(error))
      .finally(() => setSending(false));
  }

  function onSubmit(event: FormEvent) {
    event.preventDefault();
    if (ready) submit("deny");
  }

  return (
    <form aria-label="Add a rule" onSubmit={onSubmit}>
      <Inset>
        <GroupTitle>Add a rule</GroupTitle>

        <div className="flex flex-wrap items-end gap-3">
          {/*
            What KIND of rule this is, asked before the prefix because it changes what the prefix
            means: a command to run, or a path to write into. The tools the núcleo can govern are
            `classifier::WRITE_TOOLS`, and the route refuses anything else by name — an option
            here that is not on that list would be a control whose only answer is `unknown_tool`.

            `NotebookEdit` joined the list on 2026-09-08, so it is offered here now. That it had
            to be added by hand is the shape of this control: the list lives in Rust, this is a
            third copy of it after the route guard and the column CHECK, and nothing compiles the
            three together.
          */}
          <Field label="What this rule is about">
            <select
              value={tool ?? ""}
              onChange={(event) => setTool(event.target.value === "" ? null : event.target.value)}
            >
              <option value="">a command</option>
              <option value="Edit">Edit writing to a path</option>
              <option value="Write">Write writing to a path</option>
              <option value="NotebookEdit">NotebookEdit writing to a path</option>
            </select>
          </Field>
          {/*
            The label follows the choice, because with a tool selected this box holds a PATH and
            "Command prefix" would be naming it after the other kind of rule.
          */}
          <Field label={tool === null ? "Command prefix" : "Path prefix"}>
            <input
              /* The machine's face for a value the machine will store. Inline, because
                 `.ui-field input` sets `font-family: inherit` unlayered and a utility cannot win. */
              style={{ fontFamily: "var(--font-mono)" }}
              placeholder={tool === null ? "bash scripts/gates.sh" : "core/migrations"}
              value={prefix}
              spellCheck={false}
              onChange={(event) => setPrefix(event.target.value)}
            />
          </Field>
          <Field label="Why it is here">
            <input
              placeholder="the gate installs before it runs"
              value={note}
              onChange={(event) => setNote(event.target.value)}
            />
          </Field>
        </div>

        {/*
          What the núcleo will actually store, shown only where it differs from what was typed. The
          fold happens on the way IN — whitespace collapsed, ASCII lower-cased — so a form that could
          not say this is a form that surprises people.
        */}
        {folded !== "" && folded !== prefix ? (
          <p className="text-xs text-text-muted">
            stored and enforced as <span className="font-mono text-text">{folded}</span>
          </p>
        ) : null}

        {existing !== null ? (
          /*
            The Awaiting You triple in full: this asks something of the person about to press, which
            is exactly what that tone is for, and a bare foreground was a tone without its edge or
            fill.
          */
          <div className="flex max-w-[var(--measure-prose)] flex-wrap items-baseline gap-x-3 gap-y-2 rounded-md border border-tone-pending-border bg-tone-pending-bg px-3 py-2 text-xs text-tone-pending-fg">
            <p>
              <span className="font-mono">{existing.prefix}</span> is already declared as{" "}
              {existing.verdict === "allow" ? "allowed" : "refused"}, so this replaces its verdict and
              its justification rather than adding a second rule.
              {existing.note !== null && note.trim() === "" ? (
                <>
                  {" "}
                  With the justification box empty, declaring it again <strong>erases</strong> the one
                  it carries.
                </>
              ) : null}
            </p>
            {existing.note !== null && note.trim() === "" ? (
              <Button onClick={() => setNote(existing.note ?? "")}>keep its justification</Button>
            ) : null}
          </div>
        ) : null}

        <div className="flex flex-wrap items-center gap-3">
          {/*
            **Gone when a tool is chosen, and not merely disabled.** A rule about a tool can only
            refuse: the write chain in `classifier::classify` has no allow side to reach, so the route
            answers `unenforceable_allow` and `project_policy::shell_rules` would drop the row anyway.
          */}
          {tool === null ? (
            <ConfirmButton
              variant="ghost"
              label="allow it here"
              confirmLabel="allow without asking"
              subject={folded === "" ? undefined : folded}
              disabled={!ready}
              unavailable={stopped}
              title={stopped ? "The emergency stop is engaged: nothing may be allowed until it is released." : undefined}
              onConfirm={() => submit("allow")}
            />
          ) : null}
          <Button type="submit" disabled={!ready}>
            refuse it here
          </Button>
          {tool === null ? (
            <Quiet says="A refusal can name a pipe or a redirection; a permission cannot.">
              A refusal answers at the whole line and needs no shape the classifier can read, so a pipe,
              a redirection or an <span className="font-mono">-exec</span> is fine in one. A permission
              has to match a command the núcleo can take apart. Enter declares a refusal; allowing
              always asks for a second press.
            </Quiet>
          ) : (
            <Quiet says="A rule about a tool can only refuse.">
              There is nothing to allow: a write the núcleo does not refuse is already local work it
              does not stop for, so a permission here would widen nothing and be enforced by nothing.
              The path is read from the project root, and everything under it is refused with it.
            </Quiet>
          )}
        </div>

        {failed === null ? null : isApiRefusal(failed) ? (
          <RefusalNote refusal={failed} sentences={RULE_SENTENCES} />
        ) : (
          <ErrorNote>The núcleo did not answer this declaration. Nothing was stored.</ErrorNote>
        )}
      </Inset>
    </form>
  );
}

/* -------------------------------------------- 5. where the work lands -- */

/**
 * The integration branch, and the branches a `--land` may name besides it.
 *
 * **The integration branch is admissible with no row, and is drawn as a fact.** It is never in the
 * table — an empty table means "nowhere but the usual place" and not "nowhere" — so a withdraw
 * button beside it would be a control whose only possible answer is `no_such_target`. That is the
 * reading the operation tables take about an operation outside the ceiling.
 *
 * **Adding a target is one press, like narrowing, and deliberately not interlocked.** A target binds
 * nothing until a landing is attempted, and a landing goes through the queue, which the stop and
 * the queue's own grants already govern — the route is not stop-gated for the same reason.
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
 * redirected every landing"*. It is the defect `land.rs` exists to abolish, and a page is not a
 * safe place to reintroduce it.
 */
function LandTargets({ projectId }: { projectId: string }) {
  const landing = useProjectLandTargets(projectId);
  const add = useDeclareLandTarget();
  const withdraw = useForgetLandTarget();
  const writes = useRowWrites();
  const [branch, setBranch] = useState("");

  if (landing.isPending) {
    return <Reading>Reading where this project lands…</Reading>;
  }

  if (landing.data === undefined) {
    return <ReadFailed read={landing} says="where this project's work lands" />;
  }

  const { integration, targets } = landing.data;
  const wanted = branch.trim();
  const adding = writes.isPending("+");

  function onSubmit(event: FormEvent) {
    event.preventDefault();
    if (wanted === "" || adding) return;
    writes.run("+", `added ${wanted}`, () =>
      add.mutateAsync({ projectId, branch: wanted }).then(() => setBranch("")),
    );
  }

  return (
    <div className="flex flex-col gap-4">
      <Quiet says="A landing may go to the integration branch or to a branch added here; any other name is refused.">
        <span className="font-mono">nucleos-core --land &lt;branch&gt;</span> sends what is in the
        worktree it is called from, and a refused name comes back with the list of what would have
        been admissible. A branch that does not exist yet can be added: it is often made by the run
        that lands into it.
      </Quiet>

      <Inset>
        <ul className="flex flex-col gap-1">
          <li
            aria-label="land target the integration branch"
            className="flex min-h-8 flex-wrap items-baseline gap-x-3 gap-y-1 text-sm"
          >
            {integration.state === "unknown" ? (
              <span className="text-xs text-text-muted">{integration.why}</span>
            ) : (
              <>
                <span className="font-mono text-xs text-text">{integration.branch}</span>
                {/*
                  Only the `declared` arm gets the claim. A fact and not a row: the integration
                  branch needs no entry in the table to stay admissible, so there is nothing to
                  withdraw and no button is offered — and in the other two arms nothing lands by
                  default at all, so borrowing the caption would assert exactly what the núcleo would
                  refuse.
                */}
                <span className="text-xs text-text-muted">{CAPTION[integration.state]}</span>
              </>
            )}
          </li>
          {targets.map((target) => (
            <li
              key={target}
              aria-label={`land target ${target}`}
              className="flex min-h-8 flex-wrap items-baseline gap-x-3 gap-y-1 text-sm"
            >
              <span className="font-mono text-xs text-text">{target}</span>
              <span className="ml-auto flex items-baseline gap-3">
                <Receipt stamp={writes.stamp(target)} />
                <Button
                  variant="quiet"
                  disabled={writes.isPending(target)}
                  onClick={() =>
                    writes.run(target, "withdrawn", () =>
                      withdraw.mutateAsync({ projectId, branch: target }),
                    )
                  }
                >
                  withdraw
                </Button>
              </span>
              <RowFailure error={writes.failure(target)} sentences={TARGET_SENTENCES} />
            </li>
          ))}
        </ul>
      </Inset>

      <form aria-label="Add a landing target" onSubmit={onSubmit}>
        <Inset>
          <GroupTitle>Add a landing target</GroupTitle>
          <div className="flex flex-wrap items-end gap-3">
            <Field label="Branch">
              <input
                style={{ fontFamily: "var(--font-mono)" }}
                placeholder="release/next"
                value={branch}
                spellCheck={false}
                onChange={(event) => setBranch(event.target.value)}
              />
            </Field>
            <Button type="submit" disabled={wanted === "" || adding}>
              add
            </Button>
            <Receipt stamp={writes.stamp("+")} />
          </div>
          {/*
            No page copy for `unusable_branch`: its detail says which part of the spelling was refused
            — an empty name, a leading dash, whitespace — and that is what somebody has to fix.
          */}
          <RowFailure error={writes.failure("+")} sentences={TARGET_SENTENCES} />
        </Inset>
      </form>
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
  declared: "the integration branch, always admissible with or without a row",
  stale:
    "declared as the integration branch, and git cannot find that ref, so nothing lands by default until it is corrected",
  derived:
    "where a landing would go, derived from the repository. It has not been written down yet, so the first landing records it",
};

const TARGET_SENTENCES: Record<string, string> = {
  no_such_project: "the núcleo has no project by this name.",
  no_such_target:
    "no target of that name was added here. The integration branch is admissible without a row, so there is never one of those to withdraw.",
  internal: "the núcleo hit an error of its own writing it down.",
};
