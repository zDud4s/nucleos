import { useEffect, useId, useRef, useState, type ReactNode } from "react";
import { Link } from "@tanstack/react-router";
import { isApiRefusal } from "../data/client";
import {
  autonomyOf,
  rulesEditorPath,
  rulesFileName,
  useClearJudge,
  useProjectRules,
  useSetJudge,
  useSetWipLimit,
  type AutonomyRule,
  type ProjectRules,
} from "../data/projects";
import { useAssistantModels, type ModelChoice } from "../data/chats";
import {
  Button,
  ConfirmButton,
  Count,
  ErrorNote,
  Meter,
  Panel,
  Quiet,
  RefusalNote,
  RelativeTime,
  StaleNote,
  StateBadge,
} from "../ui";
import { IdeVerifyPanel } from "./IdeVerify";
/* The `pj-` family is this component's vocabulary as much as the inspector's. Imported here as
   well, so the component draws correctly wherever it is mounted — the project workspace is the
   next place, and it imports nothing from the inspector. */
import "../pages/projects.css";

/**
 * What a project does on its own, and the two brakes on it.
 *
 * Everything the inspector's `rules` view used to draw, lifted out whole so it can live where a
 * person goes to change how a project behaves. The inspector is a stop-gap for reading the tree —
 * `router.tsx` says the Código mode replaces it — and this is the one part of it that nothing
 * replaces: the rules that start work with nobody asking, the gate that measures it, the judge that
 * answers for you on Auto, and the ceiling that stops it piling up, and the IDE verify switch beside them. Safety controls found only by
 * a link from a file browser are safety controls nobody finds.
 *
 * **Self-contained on purpose.** It takes a project id and reads the rules itself. React Query
 * shares the read with any page that already made it — the inspector reads the same key for its
 * header and its concerns strip — so mounting this somewhere else costs no second request and
 * needs no prop threading.
 *
 * **It writes, and says so.** Two of the panels below change the núcleo's database: the judge and
 * the ceiling. Neither changes a file — the project's `autopilot.yaml`, which lives in
 * `~/.nucleos/projects/<id>/` and not in the project, is edited as raw text in the workspace
 * (`project/OwnedFiles.tsx`), because a form here would re-serialise the YAML and delete the
 * comments somebody left in it. This component links to that editor rather than growing a second
 * one.
 *
 * **It says when it was read.** The rules are read on open and on window focus, not on a timer
 * (`data/projects.ts` says why), so the time of the read is on the page with a way to ask again —
 * and when a later read fails, what is shown is labelled as the last good one rather than passed off
 * as current. The two writes are withdrawn while that is so: a control that acts beside a reading
 * nobody can vouch for is a control acting on a guess.
 */
export interface OnItsOwnProps {
  projectId: string;
}

export function OnItsOwn({ projectId }: OnItsOwnProps) {
  const rules = useProjectRules(projectId);

  if (rules.data === undefined) {
    if (rules.isError) {
      return (
        <Panel title="On its own">
          {isApiRefusal(rules.error) ? (
            <RefusalNote refusal={rules.error} />
          ) : (
            <ErrorNote>
              the núcleo did not answer — nothing is known about this project&apos;s rules
            </ErrorNote>
          )}
        </Panel>
      );
    }
    return (
      <Panel title="On its own">
        <p className="pj-loading">reading the rules…</p>
      </Panel>
    );
  }

  const stale = rules.isError;

  return (
    <>
      {stale && <StaleNote dataUpdatedAt={rules.dataUpdatedAt} />}
      <RulesFileState
        projectId={projectId}
        rules={rules.data}
        readAt={rules.dataUpdatedAt}
        refreshing={rules.isFetching}
        onRefresh={() => void rules.refetch()}
      />
      <Autonomy projectId={projectId} rules={rules.data} />
      <GatePanel
        projectId={projectId}
        command={rules.data.gate_command}
        beforePublish={rules.data.gate_before_publish}
      />
      <JudgePanel
        projectId={projectId}
        rules={rules.data}
        readAt={rules.dataUpdatedAt}
        stale={stale}
      />
      <WipPanel projectId={projectId} rules={rules.data} stale={stale} />
      <IdeVerifyPanel projectId={projectId} rules={rules.data} stale={stale} />
    </>
  );
}

/* ------------------------------------------------------------- shared bits -- */

/**
 * When a read landed, as the wall clock says it.
 *
 * A clock time and not "12s ago": nothing re-renders this line on a timer, so a relative phrase
 * would say "just now" for as long as the page stayed open — a claim of currency that grows less
 * true every second it is on screen. `14:02` stays exactly as true as it was. Same reading
 * `StaleNote` gives, so the two agree when both are on the page.
 *
 * Nothing at all for a read that has not landed: zero is "never", and formatting it would date the
 * page to 1970.
 */
export function ReadAt({ at }: { at: number }) {
  if (at <= 0) return null;
  const when = new Date(at);
  return (
    <span className="pj-read">
      read{" "}
      <time dateTime={when.toISOString()} title={when.toLocaleString()}>
        {when.toTimeString().slice(0, 5)}
      </time>
    </span>
  );
}

/**
 * A panel's reasoning, one line up front and the rest behind "why?".
 *
 * Every panel here used to open with a full paragraph of justification — good help for somebody
 * new, and four paragraphs re-read on every visit by somebody who is not. The lead keeps the fact;
 * the reasoning is one click away and costs no pixels until asked for. `Quiet` has this shape but
 * means "nothing here", which these panels are not, so the disclosure is its own.
 */
export function Why({ lead, children }: { lead: ReactNode; children: ReactNode }) {
  const [open, setOpen] = useState(false);
  const leadId = useId();
  const restId = useId();
  return (
    <div className="pj-why">
      <p className="pj-why-lead" id={leadId}>
        {lead}{" "}
        <button
          type="button"
          className="pj-why-ask"
          aria-expanded={open}
          aria-controls={restId}
          aria-describedby={leadId}
          onClick={() => setOpen(!open)}
        >
          {open ? "less" : "why?"}
        </button>
      </p>
      <div className="pj-why-rest" id={restId} hidden={!open}>
        {open ? children : null}
      </div>
    </div>
  );
}

/* ---------------------------------------------------------- the rules file -- */

/**
 * What state the rules file is in, and why that is information rather than a
 * fault.
 *
 * `absent` is ordinary: the file lives in `~/.nucleos/projects/<id>/`, which a
 * project gets only once somebody writes rules for it, and a project with no file
 * simply does nothing on its own.
 *
 * **Where the file is comes from the daemon** (`rules_path`), because the page
 * carried `.ai/autopilot.yaml` as a literal and was wrong the day the file moved.
 *
 * `unreadable` is the row that has to be loud. `config.rs` parses with
 * `deny_unknown_fields` precisely so a typo is an error rather than a silently
 * empty ruleset — but that error used to reach only a log line, so writing
 * `schedule:` for `schedules:` stopped all autonomy for the project and looked
 * exactly like nothing happening. The daemon's own message is the whole content
 * of that finding, so it is shown verbatim and first — and beside it the one
 * gesture that ends it, the editor, which the alert used to leave the person to
 * find on their own.
 *
 * **Not a `Panel` and not a `Badge`.** A file reads as a file: its name, its state
 * beside it, and where it is. The line also carries when it was read and a way to
 * read it again, because it is the line every reading below comes from.
 */
function RulesFileState({
  projectId,
  rules,
  readAt,
  refreshing,
  onRefresh,
}: {
  projectId: string;
  rules: ProjectRules;
  readAt: number;
  refreshing: boolean;
  onRefresh: () => void;
}) {
  const editable = rules.project_root !== null;
  const file = rulesFileName(rules);
  return (
    <div className="pj-source">
      <p className="pj-source-line">
        <code className="pj-source-name">{file}</code>
        <span className={`pj-source-state pj-source-${rules.rules_file}`}>{rules.rules_file}</span>
        <span className="pj-meta">
          {rules.project_root === null ? "no folder recorded" : `for ${rules.project_root}`}
        </span>
        {/* Once here rather than on every finding below: the unparseable file, the missing gate
            and the rule that never fires are all put right in the same file, in the same editor. */}
        {editable && rules.rules_file !== "unreadable" && (
          <Link className="pj-source-edit" to={rulesEditorPath(projectId)}>
            Edit in the workspace
          </Link>
        )}
        <span className="pj-source-read">
          <ReadAt at={readAt} />
          <Button variant="quiet" disabled={refreshing} onClick={onRefresh}>
            {refreshing ? "Reading…" : "Refresh"}
          </Button>
        </span>
      </p>
      {rules.rules_file === "unreadable" && (
        <div className="pj-rules-error" role="alert">
          <p className="pj-rules-error-title">
            The núcleo could not read this project&apos;s rules, so it is doing nothing on its own.
          </p>
          <pre className="pj-rules-error-detail">
            <code>{rules.rules_error ?? "the núcleo reported no detail"}</code>
          </pre>
          <p className="pj-note">
            The file is parsed strictly on purpose: an unknown key is an error rather than a silently
            empty ruleset. Until it parses, nothing this project might run is known — not because
            there is nothing, but because none of it could be loaded.
          </p>
          {editable && (
            <p className="pj-fix">
              <Link to={rulesEditorPath(projectId)}>Edit {file} in the workspace</Link>
            </p>
          )}
        </div>
      )}
      {rules.rules_file === "absent" && (
        <p className="pj-note">
          There is no {file} yet. That is an ordinary state and not a fault — it lives with this
          machine&rsquo;s settings rather than in the project, and exists once somebody writes rules
          — and it means this project starts nothing by itself.
        </p>
      )}
    </div>
  );
}

/* ---------------------------------------------------------- what runs here -- */

/** A clock or a commit, as a mark and a word. Never colour alone, like every mark in this app. */
const CLOCK_MARK: Record<AutonomyRule["clock"], string> = { cron: "◷", commit: "◆" };
const CLOCK_SAID: Record<AutonomyRule["clock"], string> = {
  cron: "on a clock",
  commit: "on a commit",
};

/**
 * Everything that starts work here without you, as one table.
 *
 * Two panels became one for the reason the Teams console became a table: a
 * project with two schedules and one trigger read as two half-empty lists
 * rather than as *three things run here on their own*, and a card whose blocks
 * appear only sometimes starts the next row at a different height every time. A
 * table cannot have that defect, because the columns line up by being columns.
 *
 * **Not drawn at all when the file will not parse.** The two lists it replaces
 * printed "nothing is scheduled" and "no commit starts anything here" directly
 * under an alert that had just said every rule below was absent because none
 * could be loaded.
 *
 * **Empty is one line, not a lesson.** It was a `Teach` block — the primitive for
 * when the emptiness *is* the screen — spending four hundred pixels inside a panel
 * with three more below it. `Quiet` keeps the sentence, puts the reasoning behind
 * "why?", and keeps the one gesture that would fill the space in plain sight.
 */
function Autonomy({ projectId, rules }: { projectId: string; rules: ProjectRules }) {
  if (rules.rules_file === "unreadable") return null;

  const running = autonomyOf(rules);
  if (running.length === 0) {
    return (
      <Panel title="What starts work here without you">
        <Quiet
          says="Nothing starts work here by itself."
          action={
            rules.project_root === null ? undefined : (
              <Link to={rulesEditorPath(projectId)}>Add a schedule or a trigger</Link>
            )
          }
        >
          <p>
            No schedule and no repo trigger, so this project only ever does what somebody asks it to.
            Both are written in <code>{rulesFileName(rules)}</code> — a schedule runs on a clock, a
            repo trigger runs when a branch gets a commit — and the file is edited in the project
            workspace, as text, so the comments in it survive.
          </p>
        </Quiet>
      </Panel>
    );
  }

  return (
    <Panel title="What starts work here without you" aside={<Count n={running.length} />}>
      <div className="pj-table-scroller">
        <table className="pj-table">
          <caption className="sr-only">
            Every rule that can start work in this project with nobody asking, what makes it go, and
            when it last did.
          </caption>
          <thead>
            <tr>
              <th scope="col">Rule</th>
              <th scope="col">What makes it go</th>
              <th scope="col">State</th>
              <th scope="col" className="pj-col-num">
                Next
              </th>
              <th scope="col" className="pj-col-num">
                Last
              </th>
              <th scope="col" className="pj-col-num">
                Today
              </th>
            </tr>
          </thead>
          <tbody>
            {running.map((rule) => (
              <RuleRows key={`${rule.clock}:${rule.name}`} rule={rule} />
            ))}
          </tbody>
        </table>
      </div>
    </Panel>
  );
}

function RuleRows({ rule }: { rule: AutonomyRule }) {
  return (
    <>
      <tr className={rule.problem === null ? undefined : "pj-row-problem"}>
        <th scope="row" className="pj-row-name">
          <span className="pj-row-title">
            <span className="pj-row-kind" aria-hidden="true">
              {CLOCK_MARK[rule.clock]}
            </span>
            {rule.name}
            <span className="sr-only">, {CLOCK_SAID[rule.clock]}</span>
          </span>
          {/* Always drawn, whatever its length, and clamped to one line. A field
              that appears on some rows and not others starts the next column at
              two different heights — the defect the Teams cards had. The whole
              prompt is on hover, because the clamp used to cut exactly the part
              of a sentence that said what the rule was for. */}
          <span className="pj-row-asks" title={rule.prompt}>
            {rule.prompt}
          </span>
          {rule.cwd !== null && <span className="pj-row-where">in {rule.cwd}</span>}
        </th>
        <td>
          <Trigger rule={rule} />
        </td>
        <td>
          <StateBadge domain="rule" state={rule.state} />
        </td>
        <td className="pj-col-num">
          <Moment
            at={rule.next}
            absent={
              rule.clock === "commit" || rule.state === "never-fires" ? "—" : "not scheduled"
            }
          />
        </td>
        <td className="pj-col-num">
          <Moment at={rule.last} absent={rule.clock === "commit" ? "—" : "never"} />
        </td>
        <td className="pj-col-num">
          <Today today={rule.today} />
        </td>
      </tr>
      {/*
        First-class and spanning, not a tooltip: an unparseable cron or an
        unknown timezone makes the tick skip this rule 2,880 times a day and log
        at debug, which is how a rule silently never runs. An `ErrorNote` — the
        full Wrong Red box the escalation ladder gives an error — and not a box
        with a coloured left stripe, which the system keeps for refusals only.
        The daemon's sentence stays in the mono face, because the daemon wrote it.
      */}
      {rule.problem !== null && (
        <tr>
          <td className="pj-problem-cell" colSpan={6}>
            <ErrorNote>
              <code className="pj-verbatim">{rule.problem}</code>
            </ErrorNote>
          </td>
        </tr>
      )}
    </>
  );
}

/**
 * What makes a rule go, drawn as what it is.
 *
 * A cron expression is a literal and gets a box; a branch is a name and does
 * not. They used to share one chip, so `0 3 * * *` and `main` — a schedule and
 * a git ref, which have nothing in common — wore identical clothes.
 */
function Trigger({ rule }: { rule: AutonomyRule }) {
  if (rule.clock === "commit") {
    return (
      <span className="pj-branch">
        <span className="pj-branch-what">
          a commit on <span className="pj-branch-name">{rule.when}</span>
        </span>
        {rule.sha !== null && (
          <span className="pj-branch-seen">last saw {rule.sha.slice(0, 12)}</span>
        )}
      </span>
    );
  }
  return (
    <span className="pj-when">
      <code className="pj-cron">{rule.when}</code>
      {/* `null` is UTC — the scheduler's own default, not an unset field. */}
      <span className="pj-meta">{rule.zone}</span>
    </span>
  );
}

/** A time, or the word that says why there is not one. */
function Moment({ at, absent }: { at: string | null; absent: string }) {
  if (at === null) return <span className="pj-figure pj-figure-none">{absent}</span>;
  return (
    <span className="pj-figure">
      <RelativeTime at={at} />
    </span>
  );
}

/**
 * Today's allowance, and a mark when it is spent.
 *
 * `6 / 6` and `5 / 6` are one glyph apart and are not the same news — the same
 * reason the Teams table marks a ratio at its ceiling.
 */
function Today({ today }: { today: AutonomyRule["today"] }) {
  if (today === null) return <span className="pj-figure pj-figure-none">—</span>;
  const full = today.cap > 0 && today.fired >= today.cap;
  return (
    <span className={full ? "pj-figure pj-figure-full" : "pj-figure"}>
      {today.fired}
      <span className="pj-figure-of"> / {today.cap}</span>
      {full && <span className="sr-only"> — today&apos;s allowance is spent</span>}
    </span>
  );
}

/* ------------------------------------------------------------------ the gate -- */

/**
 * What measures this project's work, and when.
 *
 * **Out of `variant="dim"`.** `dim` means "present but not the thing you came
 * for", and this panel carries the most consequential sentence on the page: a
 * queue set to wait for a gate that does not exist refuses every merge, over a
 * key in an unreviewed file. It is an `ErrorNote` — the ladder's full red box —
 * with the key in the mono face because it is a key, the sentence around it in
 * the body face because a person wrote it, and the way to the editor inside it.
 */
function GatePanel({
  projectId,
  command,
  beforePublish,
}: {
  projectId: string;
  command: string | null;
  beforePublish: boolean;
}) {
  const configured = command !== null && command.trim() !== "";
  const contradiction = beforePublish && !configured;

  return (
    <Panel title="Gate">
      {configured ? (
        <code className="pj-gate">{command}</code>
      ) : (
        /* The one absence on this page with a reason worth keeping but not worth
           reading twice. `Quiet` puts the fact on the line and the consequence one
           click behind it — the sentence is kept rather than cut, because "no gate
           is configured" alone reads as a field that failed to load rather than as
           a project nobody has gated. */
        <Quiet says="No gate is configured, so nothing measures this project’s work.">
          <p>
            That is why a job item can read <em>passed</em> with no gate status: there was nothing
            to pass.
          </p>
        </Quiet>
      )}
      {/* The second moment the same command can run, and the one nothing else on this
          page would reveal. A landing that takes twenty minutes has a reason, and the
          reason is a key in an unreviewed file — so this is where it stops being
          invisible. */}
      {contradiction ? (
        <ErrorNote>
          <span>
            <code className="pj-verbatim">gate_before_publish</code> is on and no gate command is
            set, so the queue refuses every merge.{" "}
            <Link className="pj-fix-link" to={rulesEditorPath(projectId)}>
              Set a gate command
            </Link>
          </span>
        </ErrorNote>
      ) : (
        <p className="pj-note">
          {beforePublish
            ? "Merges wait for it. The queue runs it on the merged result and publishes only if it passes; nothing is reverted, because nothing is published first."
            : "Merges do not wait for it: the queue publishes without measuring the tree the two branches make together."}
        </p>
      )}
    </Panel>
  );
}

/* ----------------------------------------------------------------- the judge -- */

type JudgeRoute = "local" | "openrouter";
type JudgeChoice = ModelChoice & { brain: JudgeRoute };

/** What a change to the judge did, said once it is done, and how to take it back. */
interface JudgeChanged {
  said: string;
  /** The control value that puts it back. `null` once it has been put back. */
  undo: string | null;
}

/**
 * How far a judge reaches, for deciding whether a change widens it.
 *
 * Nobody is nothing; the default and every local model stay on this machine; a hosted model sends
 * the command somewhere else to be judged. A change that raises this number lets something new say
 * yes on your behalf, and that is the change that takes the interlock.
 */
function reachOf(value: string, route: JudgeRoute | null): number {
  if (value === "off") return 0;
  return route === "openrouter" ? 2 : 1;
}

/**
 * Who answers for a conversation on `auto`.
 *
 * A project-level answer to a per-conversation question, and that is deliberate: the rung belongs
 * to the chat window, but WHO may say yes on your behalf belongs to the codebase being worked in.
 * A judge you trust on a scratch repository is not one you want on the thing that pays the rent.
 *
 * **Three states, and the middle one is the reason this is not a checkbox.** The default is the
 * local brain — every project has it without anybody deciding anything, and it should follow the
 * default wherever the default moves. Switched off is somebody having decided the opposite ON
 * PURPOSE, and it has to survive a change to what the default is. A two-state control would fuse
 * them and quietly re-enable a judge somebody had turned off.
 *
 * **The menu is a draft, and nothing is written until somebody says so.** It used to write on
 * `change` — and on Windows, Tab onto a closed `<select>` and one press of ↓ fires `change` without
 * opening the list, so looking at the options was enough to hand approval to a different model.
 * Now the menu only moves a draft; "Use this judge" appears when the draft differs, and a change
 * that WIDENS who answers — from nobody to anybody, or to a hosted model — takes the two-click
 * interlock. A narrowing change is one click, because taking authority away is never the dangerous
 * direction. After the write the panel says what it replaced, with an undo, in a live region: the
 * sentence above used to change silently after a refetch, which a screen reader never heard.
 *
 * The menu is the daemon's own — `GET /assistant/models`, the same read the chat window's picker
 * draws — narrowed to the two routes that can judge. `cloud` is absent because it answers through
 * the CLI, and a CLI launched to answer a hook would re-enter that hook; the daemon refuses it
 * either way, and this is the half that stops anybody having to find that out.
 *
 * **A model that declares no tool calling is not marked here, unlike in the chat picker.** A judge
 * is shown a request and a call and answers with one word; it is handed no tools and would have
 * nowhere to use them. `installed` is the mark that matters instead: a local model this machine has
 * not pulled cannot answer anything, so it is listed — seeing it is how somebody learns it can be
 * had — and not selectable.
 */
function JudgePanel({
  projectId,
  rules,
  readAt,
  stale,
}: {
  projectId: string;
  rules: ProjectRules;
  readAt: number;
  stale: boolean;
}) {
  const name = useSetJudge();
  const clear = useClearJudge();
  const models = useAssistantModels();
  const busy = name.isPending || clear.isPending;

  const judge = rules.judge;
  /* Narrowed by a predicate rather than a bare filter, so the route travels to `JudgeChange` as
     the two words that type accepts. `Brain` has a third — the one this list exists to leave out. */
  const judges = (models.data?.choices ?? []).filter(
    (choice): choice is JudgeChoice => choice.brain === "local" || choice.brain === "openrouter",
  );

  /* What the daemon holds now. The two states that name no model are their own values; a named
     judge is shown by its model. */
  const current =
    judge.state === "default"
      ? "default"
      : judge.state === "off"
        ? "off"
        : (judge.model ?? `${judge.brain}:configured`);

  /* A state this menu cannot name gets a row of its own rather than being silently redrawn as
     something else — a select whose value is absent from its options shows the FIRST option, which
     here would be a panel claiming the default while the daemon holds a judge. Two ways to reach
     one: a brain named with no model (the daemon takes it; this control never sends it), and a
     model that was on the menu when it was chosen and is not on it now — Ollama stopped, a hosted
     key withdrawn, a name removed from the file. */
  const orphan =
    judge.state === "named" && !judges.some((choice) => choice.id === current)
      ? judge.model === null
        ? `The ${judge.brain} brain, on its configured model`
        : /* "Not on the menu" is a claim about the menu, so it waits for one. Until this query
             answers — and if it never does, because the daemon went away — every model is missing
             from an empty list, and saying so about a perfectly good one would be a lie the panel
             tells for as long as the daemon is unreachable. */
          models.data !== undefined
          ? `${judge.model} — not on the menu now`
          : judge.model
      : null;

  const [draft, setDraft] = useState<string | null>(null);
  /* The value just written, and when, until a read of the rules lands after it. Without this the
     menu would jump back to the old judge for the moment between the write succeeding and the
     refetch answering — and "Use this judge" would reappear over a change already made. */
  const [sent, setSent] = useState<{ value: string; at: number } | null>(null);
  const [changed, setChanged] = useState<JudgeChanged | null>(null);

  useEffect(() => {
    if (sent !== null && readAt >= sent.at) {
      setSent(null);
      setDraft(null);
    }
  }, [readAt, sent]);

  const shown = draft ?? current;
  const differs = draft !== null && draft !== current && sent === null;

  function routeOf(value: string): JudgeRoute | null {
    if (value === "off" || value === "default") return null;
    if (value === current && judge.state === "named") return judge.brain;
    return judges.find((row) => row.id === value)?.brain ?? null;
  }

  function labelOf(value: string): string {
    if (value === "default") return "the default (the local brain)";
    if (value === "off") return "nobody but you";
    if (value === current && orphan !== null) return orphan;
    const choice = judges.find((row) => row.id === value);
    return choice === undefined ? value : `${choice.label} (${choice.brain})`;
  }

  const widens = draft !== null && reachOf(draft, routeOf(draft)) > reachOf(current, routeOf(current));

  /* One writer for the forward change and for the undo, so the two can never send different
     shapes. The brain travels WITH the model, out of the row that named both: sending the model
     alone and letting the daemon infer would be a second place that mapping lives. */
  function write(value: string, onDone: () => void) {
    const done = { onSuccess: onDone };
    if (value === "default") {
      clear.mutate(projectId, done);
      return;
    }
    if (value === "off") {
      name.mutate({ projectId, brain: null, model: null }, done);
      return;
    }
    const choice = judges.find((row) => row.id === value);
    if (choice) name.mutate({ projectId, brain: choice.brain, model: choice.id }, done);
  }

  function commit() {
    if (draft === null) return;
    const from = current;
    const fromLabel = labelOf(current);
    const to = draft;
    write(to, () => {
      setSent({ value: to, at: Date.now() });
      setChanged({ said: `Changed from ${fromLabel}.`, undo: from });
    });
  }

  function undo() {
    if (changed?.undo == null) return;
    const back = changed.undo;
    const backLabel = labelOf(back);
    setDraft(back);
    write(back, () => {
      setSent({ value: back, at: Date.now() });
      setChanged({ said: `Put back: ${backLabel} answers again.`, undo: null });
    });
  }

  return (
    <Panel title="Judge">
      <Why lead="A judge answers, in your place, what a conversation on Auto would otherwise ask you.">
        <p>
          A conversation on <strong>Auto</strong> stops and asks about anything its rules do not
          recognise. A judge is what answers those in your place — a model, given the command and
          nothing else, inside the same window you would have had to answer in.
        </p>
      </Why>

      <p className="pj-wip-state">
        {judge.state === "default" ? (
          <>
            The <strong>local</strong> brain answers, on whatever model it is configured with.
            Nobody has chosen otherwise for this project.
          </>
        ) : judge.state === "off" ? (
          <>
            <strong>Nobody</strong> answers but you. Every question a conversation on Auto raises
            here waits for a person.
          </>
        ) : (
          <>
            The <strong>{judge.brain}</strong> brain answers
            {judge.model === null ? (
              <>, on its configured model.</>
            ) : (
              <>
                , on <code className="pj-gate">{judge.model}</code>.
              </>
            )}
          </>
        )}
      </p>

      {/* Always in the document, so the change that lands in it is announced: a live region
          inserted together with its text is one most screen readers never read. */}
      <div className="pj-changed" role="status">
        {changed !== null && (
          <p className="pj-changed-said">
            {changed.said}{" "}
            {changed.undo !== null && (
              <Button variant="link" disabled={busy || stale} onClick={undo}>
                Undo
              </Button>
            )}
          </p>
        )}
      </div>

      {stale ? (
        <p className="pj-note">
          The judge can be changed again once the rules read is current — changing it beside a
          reading nobody can vouch for would be deciding on a guess.
        </p>
      ) : (
        <div className="pj-form">
          <label className="pj-field-label" htmlFor="pj-judge">
            Who answers
          </label>
          <select
            id="pj-judge"
            className="pj-field-input pj-field-select"
            value={shown}
            disabled={busy}
            onChange={(event) => {
              setChanged(null);
              setDraft(event.target.value === current ? null : event.target.value);
            }}
          >
            {orphan !== null && <option value={current}>{orphan}</option>}
            <option value="default">The default — the local brain, on its configured model</option>
            <option value="off">Nobody but me</option>
            {(["local", "openrouter"] as const).map((route) => {
              const rows = judges.filter((choice) => choice.brain === route);
              if (rows.length === 0) return null;
              return (
                <optgroup key={route} label={route === "local" ? "Local" : "OpenRouter"}>
                  {rows.map((choice) => (
                    <option key={choice.id} value={choice.id} disabled={choice.installed === false}>
                      {choice.label}
                      {choice.installed === false ? " — not downloaded" : ""}
                    </option>
                  ))}
                </optgroup>
              );
            })}
          </select>

          {differs && (
            <div className="pj-actions">
              {widens ? (
                <ConfirmButton
                  variant="approve"
                  label="Use this judge"
                  confirmLabel={`Let ${labelOf(draft)} answer for you`}
                  disabled={busy}
                  onConfirm={commit}
                />
              ) : (
                <Button variant="ghost" disabled={busy} onClick={commit}>
                  Use this judge
                </Button>
              )}
              <Button variant="quiet" disabled={busy} onClick={() => setDraft(null)}>
                Keep the current one
              </Button>
            </div>
          )}
        </div>
      )}

      {/* The daemon's own sentence, not one written here. It knows which model and which route,
          and the two refusals somebody actually meets — a hosted model with no key stored, a local
          one this machine cannot serve — are facts about this machine that no copy in the window
          could keep current. */}
      {name.isError &&
        (isApiRefusal(name.error) ? (
          <RefusalNote refusal={name.error} />
        ) : (
          <ErrorNote>the núcleo did not answer — the judge is unchanged</ErrorNote>
        ))}
      {clear.isError && !isApiRefusal(clear.error) && (
        <ErrorNote>the núcleo did not answer — the judge is unchanged</ErrorNote>
      )}
    </Panel>
  );
}

/* --------------------------------------------------------------- the ceiling -- */

/** A ceiling as words, for the line that says what a change replaced. */
function ceilingSaid(limit: number | null): string {
  return limit === null ? "no ceiling" : String(limit);
}

/**
 * The WIP ceiling — one of the two writes this component makes, the judge being the other.
 *
 * `null` is the brake **off** and is never drawn as `0`: the daemon compares
 * `open >= limit`, so a ceiling of zero would mean *never start anything again*
 * while reading like a number somebody chose. The two are one keystroke apart in
 * a form and opposite in effect, so the form keeps them apart — a number field
 * for a ceiling, a separate control for turning the brake off.
 *
 * Plain buttons rather than the two-click interlock, for every ceiling but one: a
 * ceiling is a number that can be typed again in five seconds, and `ConfirmButton`
 * exists for what cannot be undone. **Zero is the exception.** It stops every new
 * piece of autonomous work in the project, it was accepted in silence and behind
 * the green button, and nothing about the field said so; now the page says what it
 * will do before it is set, and setting it takes the interlock.
 *
 * "Set the ceiling" is `ghost`, not `approve`: `approve` is the one affirmative fill
 * in the system, and spending it on a number field teaches that green means "click
 * here" rather than "yes, let it happen".
 *
 * The reading above the form is a `Meter`: a count against a ceiling with a state
 * of full, which is the exact thing that primitive draws — including the one
 * distinction this panel exists to defend: `ceiling: null` is a dashed rail reading
 * "no ceiling", which cannot be mistaken for either an empty bar or a full one.
 */
function WipPanel({
  projectId,
  rules,
  stale,
}: {
  projectId: string;
  rules: ProjectRules;
  stale: boolean;
}) {
  const setLimit = useSetWipLimit();
  const zeroId = useId();
  const [draft, setDraft] = useState(rules.wip_limit === null ? "" : String(rules.wip_limit));
  const [changed, setChanged] = useState<string | null>(null);
  const parsed = Number.parseInt(draft.trim(), 10);
  const valid = Number.isSafeInteger(parsed) && parsed >= 0;
  const zero = valid && parsed === 0;
  /* The ceiling a write replaced, captured at the moment it was sent — `rules` is re-read after
     the write, and by the time the success lands it may already hold the new value. */
  const before = useRef(rules.wip_limit);

  function send(limit: number | null) {
    before.current = rules.wip_limit;
    setChanged(null);
    setLimit.mutate(
      { projectId, limit },
      {
        onSuccess: () =>
          setChanged(
            limit === null
              ? `Ceiling removed — it was ${ceilingSaid(before.current)}.`
              : `Ceiling set to ${limit} — it was ${ceilingSaid(before.current)}.`,
          ),
      },
    );
  }

  return (
    <Panel title="Work-in-progress ceiling">
      <Why lead="How much unreviewed work this project may hold before it stops starting more.">
        <p>
          The brake is self-clearing — it releases the moment you review something — so this is the
          answer to &ldquo;how much unanswered work am I willing to have open&rdquo;, not a quota.
        </p>
      </Why>

      {/* Open work occupies the ceiling; a full queue asks the reader to review it. */}
      <Meter
        label="open and unreviewed"
        value={rules.open_review_items}
        ceiling={rules.wip_limit}
        tone={rules.queue_full ? "pending" : "active"}
      />

      <p className="pj-wip-state">
        {rules.wip_limit === null ? (
          <>
            The brake is <strong>off</strong>: no ceiling at all, which is not the same as a ceiling
            of zero.
          </>
        ) : rules.queue_full ? (
          <>
            It is currently holding new autonomous work back.{" "}
            <Link className="pj-fix-link" to="/waiting">
              Review what is waiting
            </Link>
          </>
        ) : (
          <>Nothing is being held back by it.</>
        )}
      </p>

      <div className="pj-changed" role="status">
        {changed !== null && <p className="pj-changed-said">{changed}</p>}
      </div>

      {stale ? (
        <p className="pj-note">
          The ceiling can be changed again once the rules read is current.
        </p>
      ) : (
        <div className="pj-form">
          <label className="pj-field-label" htmlFor="pj-wip">
            Ceiling
          </label>
          <input
            id="pj-wip"
            className="pj-field-input pj-field-number"
            type="number"
            min={0}
            step={1}
            value={draft}
            aria-describedby={zero ? zeroId : undefined}
            onChange={(event) => setDraft(event.target.value)}
          />
          {zero && (
            <p className="pj-warn" id={zeroId}>
              A ceiling of 0 means this project starts nothing new on its own until you raise it.
            </p>
          )}
          <div className="pj-actions">
            {zero ? (
              <ConfirmButton
                variant="ghost"
                label="Set the ceiling"
                confirmLabel="Set 0 — nothing new starts here"
                disabled={setLimit.isPending}
                onConfirm={() => send(0)}
              />
            ) : (
              <Button
                variant="ghost"
                disabled={!valid || setLimit.isPending}
                onClick={() => send(parsed)}
              >
                Set the ceiling
              </Button>
            )}
            <Button
              variant="ghost"
              disabled={rules.wip_limit === null || setLimit.isPending}
              onClick={() => {
                setDraft("");
                send(null);
              }}
            >
              Remove the ceiling
            </Button>
          </div>
        </div>
      )}

      {draft.trim() !== "" && !valid && (
        <p className="pj-note">
          A ceiling is a whole number, zero or more. The núcleo refuses a negative one outright — it
          would read like a number somebody chose and mean &ldquo;never start anything again&rdquo;.
        </p>
      )}
      {setLimit.isError && <WipError error={setLimit.error} />}
    </Panel>
  );
}

function WipError({ error }: { error: unknown }) {
  if (!isApiRefusal(error)) {
    return <ErrorNote>the núcleo did not answer — the ceiling is unchanged</ErrorNote>;
  }
  return (
    <RefusalNote
      refusal={error}
      sentences={{
        not_found: "the núcleo has no row for this project, so there is no ceiling to set on it",
        bad_request: "a ceiling cannot be negative",
      }}
    />
  );
}
