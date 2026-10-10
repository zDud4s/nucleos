import { useEffect, useState } from "react";
import type { AgentEngine, AgentRequest, AgentToolPolicy } from "../data/agents";
import { isApiRefusal } from "../data/client";
import type { Proposal } from "../data/system";
import { useRejectProposal } from "../data/waiting";
import {
  parseActionPayload,
  teamActionState,
  useApproveTeamAction,
  useHireRecruit,
  useOpenTeamActions,
  useRecruitProposals,
  type TeamAction,
  type TeamRun,
  type TeamView,
} from "../data/teams";
import {
  Badge,
  Button,
  ConfirmButton,
  Count,
  ErrorNote,
  Field,
  Inset,
  Panel,
  Quiet,
  RefusalNote,
  RelativeTime,
  StateBadge,
  Teach,
} from "../ui";
import { daemonProse } from "./prose";

/**
 * `Decisions` — what this department is waiting on a person for.
 *
 * Both of these exist today only in the machine-wide queue at `/waiting`, which
 * is the right place to work through everything at once and the wrong place to
 * answer one department's question: the context that makes the answer obvious —
 * what this department is for, who is on it, what it already may do without
 * asking — is on the other page. Bringing the decision to the context is the
 * house precedent (`get_contact_merges`, `core/src/http.rs:1239`); this does the
 * same for a department.
 *
 * The two sections are two different acts and share no button:
 *
 * - **An action** is *do that*. It executes once and is finished.
 * - **A recruitment** is *keep this person*. It executes nothing and lasts
 *   forever — and it is the only proposal in the house that is **editable at
 *   the moment of decision**, which is why its button says **Hire** rather than
 *   Approve (`core/src/http.rs:1223`, `proposals.rs:449`).
 *
 * ## What the counts here can and cannot claim
 *
 * `GET /team-actions` answers `pending` **and** `working`
 * (`core/src/team.rs:3570`), while the daemon's own ceiling counts only
 * `pending` with an undecided proposal (`open_actions_of`, `team.rs:1764`). So
 * anything counted on this tab is an upper bound on what the daemon would call
 * open, and the copy says "waiting" rather than a number the daemon would
 * recognise.
 */

export interface DecisionsProps {
  team: TeamView;
  /** Every run the window holds, for `team_run_id → team_id`. Not filtered by the caller. */
  runs: TeamRun[];
}

export function Decisions({ team, runs }: DecisionsProps) {
  const actions = useOpenTeamActions();
  const recruits = useRecruitProposals();

  const mine = new Set(runs.filter((run) => run.team_id === team.id).map((run) => run.id));
  const theirs = (actions.data ?? []).filter((action) => mine.has(action.team_run_id));
  const asked = (recruits.data ?? []).filter((proposal) => recruitTeam(proposal) === team.id);

  return (
    <div className="teams-decisions">
      <Teach title="Why only some actions appear here">
        <p>
          The limit here is <strong>your attention</strong>, not the team&apos;s capacity:
          only the actions it has been granted as <em>asks first</em> ever queue. Anything granted
          as <em>does it</em> happens within about ten seconds and never appears — change that on
          the Charter tab, under Powers.
        </p>
      </Teach>

      <Panel title="Actions it wants to take" aside={<Count n={actions.data === undefined ? undefined : theirs.length} />}>
        {actions.isError && actions.data === undefined && (
          <ErrorNote>NucleOS did not answer, so nothing is known about what is waiting.</ErrorNote>
        )}
        {actions.isLoading && <Quiet says="Asking NucleOS…" />}
        {actions.data !== undefined && theirs.length === 0 && (
          <Quiet says="Nothing is waiting on you for this team." />
        )}
        {theirs.length > 0 && (
          <ul className="teams-acts" aria-label="Actions">
            {theirs.map((action) => (
              <ActionCard key={action.id} action={action} />
            ))}
          </ul>
        )}
      </Panel>

      <Panel title="Specialists it asked for" aside={<Count n={recruits.data === undefined ? undefined : asked.length} />}>
        {asked.length > 0 && (
          <p className="teams-note">
            A director found a gap in its roster. The request is editable here before it is
            granted; saying not now leaves nothing behind, and the team may ask again.
          </p>
        )}
        {recruits.isError && recruits.data === undefined && (
          <ErrorNote>
            Couldn&apos;t load the requested specialists. They&apos;ll show up when NucleOS answers.
          </ErrorNote>
        )}
        {recruits.isLoading && <Quiet says="Asking NucleOS…" />}
        {recruits.data !== undefined && asked.length === 0 && (
          <Quiet says="This team hasn't asked for anybody." />
        )}
        {asked.length > 0 && (
          <ul className="teams-acts" aria-label="Recruitment">
            {asked.map((proposal) => (
              <RecruitCard key={proposal.id} proposal={proposal} />
            ))}
          </ul>
        )}
      </Panel>
    </div>
  );
}

/**
 * Which department a recruitment belongs to.
 *
 * From `team_id` inside the JSON `tool_input`, which is where
 * `create_agent_recruit` puts it (`core/src/proposals.rs:464`) and where
 * `hire_recruit` reads it back from to decide whose roster to add to
 * (`core/src/team.rs:1684`). Reading the same field the hire reads is what keeps
 * this filter and that write agreeing.
 *
 * Note the daemon's own `team_run_id` filter on `GET /proposals/recruits` is a
 * *different* filter for a different caller — a director's own prompt, scoped to
 * one run. This is scoped to a department, which is a wider question, and the
 * route takes no parameter for it. Hence: filtered here.
 */
export function recruitTeam(proposal: Proposal): string | null {
  if (proposal.tool_input === null) return null;
  const payload = parseActionPayload(proposal.tool_input);
  const named = payload?.team_id;
  return typeof named === "string" ? named : null;
}

/* -------------------------------------------------------------- actions -- */

/**
 * One action, rendered by what it actually is.
 *
 * A JSON blob is not a decision anybody can make. The three grantable kinds get
 * the three fields that decide them — who an email is to and what it says, when
 * a calendar entry is, what path a document is written to — and anything else
 * falls back to the raw payload rather than to a guess.
 *
 * `why` is a quotation because it is the director's own sentence, and it is the
 * thing a person actually reads before answering.
 */
function ActionCard({ action }: { action: TeamAction }) {
  const approve = useApproveTeamAction();
  const reject = useRejectProposal();
  const payload = parseActionPayload(action.payload);

  return (
    <Inset as="li">
      <div className="teams-act-head">
        <span className="teams-act-kind">{action.kind}</span>
        <StateBadge domain="team_action" state={teamActionState(action)} />
        <RelativeTime at={action.created_at} />
        {/* `null` means the grant was `allow`: nobody decides and it happens on
            the next tick (ten seconds). It is here to be seen, not to be answered. */}
        {action.proposal_id === null && <Badge tone="info">granted — nobody decides</Badge>}
      </div>

      <blockquote className="teams-act-why">{action.why}</blockquote>
      <ActionPayload kind={action.kind} payload={payload} raw={action.payload} />

      {action.proposal_id !== null && action.state === "pending" && (
        <div className="teams-act-controls">
          <ConfirmButton
            label="Approve"
            confirmLabel="Let it do this"
            variant="approve"
            disabled={approve.isPending}
            onConfirm={() => approve.mutate(action.proposal_id as number)}
          />
          <Button
            disabled={reject.isPending}
            onClick={() => reject.mutate(action.proposal_id as number)}
          >
            Not this
          </Button>
        </div>
      )}

      {approve.isSuccess && (
        <p className="teams-act-outcome" role="status">
          {/* Approving says yes and nothing else — the daemon carries it out on
              its next tick, which is why the answer is `queued`, not a result. */}
          Said yes. NucleOS carries it out within about ten seconds.
        </p>
      )}
      {reject.isSuccess && (
        <p className="teams-act-outcome" role="status">
          Refused — it won&apos;t happen.
        </p>
      )}
      {approve.isError && <DecisionRefusal error={approve.error} what="this was not approved" />}
      {reject.isError && <DecisionRefusal error={reject.error} what="this was not refused" />}
    </Inset>
  );
}

/** The fields that decide each kind, and the raw payload for anything else. */
function ActionPayload({
  kind,
  payload,
  raw,
}: {
  kind: string;
  payload: Record<string, unknown> | null;
  raw: string;
}) {
  if (payload === null) {
    // A payload we cannot parse is shown verbatim. A worse view, not a broken one.
    return <pre className="teams-act-raw">{raw}</pre>;
  }

  const fields = FIELDS_OF[kind];
  if (fields === undefined) return <pre className="teams-act-raw">{raw}</pre>;

  return (
    <ul className="teams-act-payload">
      {fields.map(([key, label]) => {
        const value = payload[key];
        if (value === undefined || value === null) return null;
        return (
          <li className="teams-act-field" key={key}>
            <span className="teams-act-field-name">{label}</span>
            <span>{typeof value === "string" ? value : JSON.stringify(value)}</span>
          </li>
        );
      })}
    </ul>
  );
}

/**
 * The three grantable kinds and what decides each of them.
 *
 * `GRANTABLE_ACTIONS` is closed at three (`core/src/team.rs:368`), so this table
 * is closed at three too — and a fourth kind arriving falls through to the raw
 * payload rather than being rendered as a guess.
 */
const FIELDS_OF: Record<string, [string, string][]> = {
  send_email: [
    ["to", "to"],
    ["subject", "subject"],
    ["body", "body"],
  ],
  calendar_event: [
    ["title", "title"],
    ["starts_at", "when"],
    ["ends_at", "until"],
    ["location", "where"],
  ],
  file_document: [
    ["path", "path"],
    ["title", "title"],
  ],
};

/* ------------------------------------------------------------ recruits -- */

/** Every editable field of a recruitment, seeded once from the proposed `AgentRequest`. */
function seedRecruit(raw: string | null): AgentRequest | null {
  if (raw === null) return null;
  const parsed = parseActionPayload(raw);
  if (parsed === null) return null;
  const text = (value: unknown): string => (typeof value === "string" ? value : "");
  return {
    name: text(parsed.name),
    speciality: text(parsed.speciality),
    prompt: text(parsed.prompt),
    engine: text(parsed.engine),
    model: typeof parsed.model === "string" ? parsed.model : null,
    tool_policy: text(parsed.tool_policy),
  };
}

/**
 * A specialist a director asked for — editable before it is granted.
 *
 * The same treatment `Waiting.tsx` gives it, and deliberately not a new one: the
 * six fields, seeded **once** so a poll tick cannot overwrite half a correction,
 * with engine and tool policy called out because they are the two a director
 * gets wrong most often and the two that cost money and reach.
 *
 * Hiring here changes the roster **outside the Charter form**, which is exactly
 * the drift the Charter's guard exists to catch — see `drift.ts`.
 */
function RecruitCard({ proposal }: { proposal: Proposal }) {
  const hire = useHireRecruit();
  const reject = useRejectProposal();
  const [form, setForm] = useState<AgentRequest | null>(null);

  useEffect(() => {
    if (form !== null) return;
    setForm(seedRecruit(proposal.tool_input));
  }, [form, proposal.tool_input]);

  function field<K extends keyof AgentRequest>(key: K, value: AgentRequest[K]) {
    setForm((current) => (current === null ? current : { ...current, [key]: value }));
  }

  const idFor = (name: string) => `team-recruit-${proposal.id}-${name}`;

  return (
    <Inset as="li">
      <div className="teams-act-head">
        <span className="teams-act-kind" title={`Recruitment #${proposal.id}`}>
          specialist requested
        </span>
        <RelativeTime at={proposal.created_at} />
      </div>
      <blockquote className="teams-act-why">
        {proposal.reasoning.trim() === "" ? "Nothing was recorded about why." : proposal.reasoning}
      </blockquote>

      {form === null ? (
        <pre className="teams-act-raw">{proposal.tool_input ?? "Nothing was proposed."}</pre>
      ) : (
        <div className="teams-hire">
          <Field label="Name">
            <input
              className="teams-input"
              id={idFor("name")}
              value={form.name}
              onChange={(event) => field("name", event.target.value)}
            />
          </Field>
          <Field label="Speciality">
            <input
              className="teams-input"
              id={idFor("speciality")}
              value={form.speciality}
              onChange={(event) => field("speciality", event.target.value)}
            />
          </Field>
          <Field label="Prompt">
            <textarea
              className="teams-textarea"
              id={idFor("prompt")}
              rows={4}
              value={form.prompt}
              onChange={(event) => field("prompt", event.target.value)}
            />
          </Field>
          <Field
            label="Engine"
            helper="Directors most often get the engine and tool policy wrong: the engine sets what each turn costs, and the tool policy sets what this specialist can reach."
          >
            <select
              className="teams-select"
              id={idFor("engine")}
              value={form.engine}
              onChange={(event) => field("engine", event.target.value)}
            >
              <OptionsWith current={form.engine} known={RECRUIT_ENGINES} />
            </select>
          </Field>
          <Field label="Model">
            <input
              className="teams-input teams-input-num"
              id={idFor("model")}
              placeholder="engine default"
              value={form.model ?? ""}
              onChange={(event) => field("model", event.target.value === "" ? null : event.target.value)}
            />
          </Field>
          <Field label="Tool policy">
            <select
              className="teams-select"
              id={idFor("tool_policy")}
              value={form.tool_policy}
              onChange={(event) => field("tool_policy", event.target.value)}
            >
              <OptionsWith current={form.tool_policy} known={RECRUIT_TOOL_POLICIES} />
            </select>
          </Field>
        </div>
      )}

      <div className="teams-act-controls">
        {/* "Hire", not "Approve": approving an action means do that once;
            hiring means keep this person, and it is written over whatever was
            edited above. */}
        <ConfirmButton
          label={`Hire ${form?.name || "specialist"}`}
          confirmLabel="Write the specialist and add them to the roster"
          variant="approve"
          disabled={hire.isPending || form === null}
          onConfirm={() => {
            if (form !== null) hire.mutate({ proposalId: proposal.id, hire: form });
          }}
        />
        <Button disabled={reject.isPending} onClick={() => reject.mutate(proposal.id)}>
          Not now
        </Button>
      </div>

      {hire.isSuccess && hire.data !== undefined && (
        <p className="teams-act-outcome" role="status">
          {hire.data.agent_id} is hired and on this team&apos;s roster.
        </p>
      )}
      {reject.isSuccess && (
        <p className="teams-act-outcome" role="status">
          Refused — it won&apos;t happen.
        </p>
      )}
      {hire.isError && <DecisionRefusal error={hire.error} what="nobody was hired" />}
      {reject.isError && <DecisionRefusal error={reject.error} what="this was not refused" />}
    </Inset>
  );
}

/** What the catalogue accepts for `engine` and `tool_policy` — the same lists `Agents.tsx` offers. */
const RECRUIT_ENGINES: AgentEngine[] = ["claude", "codex", "local"];
const RECRUIT_TOOL_POLICIES: AgentToolPolicy[] = ["mcp_only", "none"];

/**
 * A select's options, keeping whatever the director proposed even when it is not one of them.
 *
 * A director can propose a value the daemon refuses (`read_only` is the usual one). Dropping it
 * would show the first option as if it had been proposed; keeping it, marked, shows what was
 * asked for and lets the hire's refusal say why. (Same treatment as `Waiting.tsx`.)
 */
function OptionsWith({ current, known }: { current: string; known: string[] }) {
  return (
    <>
      {!known.includes(current) && (
        <option value={current}>
          {current === "" ? "none proposed" : `${current} (not accepted)`}
        </option>
      )}
      {known.map((value) => (
        <option key={value} value={value}>
          {value}
        </option>
      ))}
    </>
  );
}

/**
 * Why a decision did not go through.
 *
 * The daemon's own sentence wins here — a hire refused for a name that collided
 * while the request waited says exactly what to do about it, and the shared
 * floor sentence for `conflict` does not.
 */
function DecisionRefusal({ error, what }: { error: unknown; what: string }) {
  if (isApiRefusal(error)) return <RefusalNote refusal={error} sentences={daemonProse(error)} />;
  return <ErrorNote>NucleOS did not answer, so {what}.</ErrorNote>;
}
