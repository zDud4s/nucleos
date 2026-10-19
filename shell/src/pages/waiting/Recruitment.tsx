import { useEffect, useMemo, useState } from "react";
import type { AgentEngine, AgentRequest, AgentToolPolicy } from "../../data/agents";
import type { Proposal } from "../../data/system";
import { parseActionPayload, useHireRecruit } from "../../data/teams";
import { useRejectProposal } from "../../data/waiting";
import { Button, ConfirmButton, Inset, RelativeTime, Row, Rows } from "../../ui";
import type { Reading } from "./reading";
import { DecisionRefusal, QUEUES, Queue, Reasoning, dense, useOrderFreeze } from "./shared";
import { DepartmentContext } from "./TeamActions";

/* ---------------------------------------------------------- 4. recruitment -- */

/**
 * The specialists directors ask for, editable at the moment of decision.
 *
 * Unlike everything else in this queue, this is the one approval a person may
 * correct before granting it — `agent::validate` runs over what was approved,
 * not over what was proposed.
 */
export function RecruitmentSection({ view }: { view: Reading<Proposal> }) {
  const rows = view.rows ?? [];
  const { items, onArmedChange } = useOrderFreeze(
    rows,
    (proposal) => proposal.id,
  );

  return (
    <Queue
      title={QUEUES.recruits.title}
      view={view}
      what="the recruitment requests"
      count={items.length}
      says={QUEUES.recruits.says}
    >
      <Rows label={QUEUES.recruits.title} className={dense(items.length) ? "waiting-dense" : undefined}>
        {items.map((proposal) => (
          <RecruitmentCard
            key={proposal.id}
            proposal={proposal}
            count={items.length}
            onArmedChange={onArmedChange}
          />
        ))}
      </Rows>
    </Queue>
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
 * asked for and lets the hire's refusal say why.
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

/** Every editable field of a recruitment, seeded once from the proposed `AgentRequest`. */
function seedRecruitForm(raw: string | null): AgentRequest | null {
  if (raw === null) return null;
  const parsed = parseActionPayload(raw);
  if (parsed === null) return null;
  const text = (value: unknown): string =>
    typeof value === "string" ? value : "";
  return {
    name: text(parsed.name),
    speciality: text(parsed.speciality),
    prompt: text(parsed.prompt),
    engine: text(parsed.engine),
    model: typeof parsed.model === "string" ? parsed.model : null,
    tool_policy: text(parsed.tool_policy),
  };
}

function RecruitmentCard({
  proposal,
  count,
  onArmedChange,
}: {
  proposal: Proposal;
  /** How many rows the list has, which is what decides how tight this one is. */
  count: number;
  onArmedChange: (armed: boolean) => void;
}) {
  const hire = useHireRecruit();
  const reject = useRejectProposal();
  // Seed once: a poll tick landing mid-edit must not overwrite what the person
  // has already typed, so the effect only ever sets the form while it is null.
  const [form, setForm] = useState<AgentRequest | null>(null);
  // The proposal text the form was seeded from. A later poll can bring a different one (the
  // director revised the request); the form stays as typed, and this is what says it is behind.
  const [seededFrom, setSeededFrom] = useState<string | null>(null);
  useEffect(() => {
    if (form !== null) return;
    setForm(seedRecruitForm(proposal.tool_input));
    setSeededFrom(proposal.tool_input);
  }, [form, proposal.tool_input]);
  const proposalChanged = form !== null && proposal.tool_input !== seededFrom;
  const proposed = useMemo(() => seedRecruitForm(seededFrom), [seededFrom]);

  function loadChangedProposal() {
    setForm(seedRecruitForm(proposal.tool_input));
    setSeededFrom(proposal.tool_input);
    // The reader asked for the new values: put them where the edit starts.
    window.requestAnimationFrame(() =>
      document.getElementById(`recruit-${proposal.id}-name`)?.focus(),
    );
  }

  function field<K extends keyof AgentRequest>(key: K, value: AgentRequest[K]) {
    setForm((current) =>
      current === null ? current : { ...current, [key]: value },
    );
  }

  const idFor = (name: string) => `recruit-${proposal.id}-${name}`;
  // The caution about engine and tool policy used to be a paragraph between the fields, on
  // every card, all the time. It is help for two fields, so it belongs to those two fields:
  // read out when either takes focus, and on hover for anybody with a pointer.
  const carefulId = idFor("careful");
  const carefulChanged =
    form !== null &&
    proposed !== null &&
    (form.engine !== proposed.engine || form.tool_policy !== proposed.tool_policy);
  const careful =
    "Engine and tool policy are the two fields a director gets wrong most often — they are also what costs money per turn and what widens what this agent can reach.";

  return (
    <Row className="waiting-card" dense={dense(count)}>
      <div className="waiting-card-head">
        <span className="waiting-card-id" tabIndex={-1} data-waiting-heading>recruit #{proposal.id}</span>
        <RelativeTime at={proposal.created_at} />
      </div>
      <DepartmentContext proposal={proposal} />
      <Reasoning>
        {proposal.reasoning.trim() === ""
          ? "nothing was recorded about why"
          : proposal.reasoning}
      </Reasoning>
      {form === null ? (
        <p className="waiting-payload-raw">
          {proposal.tool_input ?? "nothing was proposed"}
        </p>
      ) : (
        // The one card in this queue that is a form. The inset is what says the
        // fields are a thing standing on the row rather than more of the row.
        <Inset>
          {proposalChanged && (
            <p className="waiting-notice waiting-recruit-changed" role="status">
              The director changed this proposal after you opened it. The fields below still hold
              the earlier values.{" "}
              <Button variant="quiet" onClick={loadChangedProposal}>
                Load the new proposal
              </Button>
            </p>
          )}
          <div className="waiting-recruit">
            <div className="waiting-recruit-form">
              <div className="waiting-recruit-field">
                <label className="waiting-recruit-label" htmlFor={idFor("name")}>
                  Name
                </label>
                <input
                  className="waiting-recruit-input"
                  id={idFor("name")}
                  value={form.name}
                  onChange={(event) => field("name", event.target.value)}
                />
              </div>
              <div className="waiting-recruit-field waiting-recruit-span-2">
                <label className="waiting-recruit-label" htmlFor={idFor("speciality")}>
                  Speciality
                </label>
                <input
                  className="waiting-recruit-input"
                  id={idFor("speciality")}
                  value={form.speciality}
                  onChange={(event) => field("speciality", event.target.value)}
                />
              </div>
              <div className="waiting-recruit-field waiting-recruit-span-all">
                <label className="waiting-recruit-label" htmlFor={idFor("prompt")}>
                  Prompt
                </label>
                <textarea
                  className="waiting-recruit-textarea"
                  id={idFor("prompt")}
                  rows={4}
                  value={form.prompt}
                  onChange={(event) => field("prompt", event.target.value)}
                />
              </div>
              <div className="waiting-recruit-field">
                <label className="waiting-recruit-label" htmlFor={idFor("engine")}>
                  Engine
                </label>
                <select
                  className="waiting-recruit-input waiting-recruit-input-careful"
                  id={idFor("engine")}
                  aria-describedby={carefulId}
                  value={form.engine}
                  onChange={(event) => field("engine", event.target.value)}
                >
                  <OptionsWith current={form.engine} known={RECRUIT_ENGINES} />
                </select>
              </div>
              <div className="waiting-recruit-field">
                <label className="waiting-recruit-label" htmlFor={idFor("model")}>
                  Model
                </label>
                <input
                  className="waiting-recruit-input"
                  id={idFor("model")}
                  placeholder="engine default"
                  value={form.model ?? ""}
                  onChange={(event) =>
                    field(
                      "model",
                      event.target.value === "" ? null : event.target.value,
                    )
                  }
                />
              </div>
              <div className="waiting-recruit-field">
                <label className="waiting-recruit-label" htmlFor={idFor("tool_policy")}>
                  Tool policy
                </label>
                <select
                  className="waiting-recruit-input waiting-recruit-input-careful"
                  id={idFor("tool_policy")}
                  aria-describedby={carefulId}
                  value={form.tool_policy}
                  onChange={(event) => field("tool_policy", event.target.value)}
                >
                  <OptionsWith current={form.tool_policy} known={RECRUIT_TOOL_POLICIES} />
                </select>
              </div>
              {/* Visible, not a `title`: this is what costs money per turn, and a tooltip waits,
                  never shows on touch and is not read by most readers. It is also the two
                  fields' description. */}
              <p
                className={
                  carefulChanged
                    ? "waiting-recruit-careful waiting-recruit-careful-changed waiting-recruit-span-all"
                    : "waiting-recruit-careful waiting-recruit-span-all"
                }
                id={carefulId}
              >
                {careful}
                {carefulChanged && " Changed from what the director proposed."}
              </p>
            </div>
          </div>
        </Inset>
      )}
      <div className="waiting-actions-split">
        <ConfirmButton
          label="Hire"
          confirmLabel="Hire and add them to the roster"
          variant="approve"
          disabled={hire.isPending || form === null}
          onArmedChange={onArmedChange}
          onConfirm={() => {
            if (form !== null)
              hire.mutate({ proposalId: proposal.id, hire: form });
          }}
        />
        <ConfirmButton
          label="Decline"
          confirmLabel="Decline — the team may ask again"
          variant="ghost"
          disabled={reject.isPending}
          onArmedChange={onArmedChange}
          onConfirm={() => reject.mutate(proposal.id)}
        />
      </div>
      {hire.isSuccess && hire.data !== undefined && (
        <p className="waiting-outcome" role="status">
          agent {hire.data.agent_id} is hired and on the roster
        </p>
      )}
      {hire.isError && <DecisionRefusal error={hire.error} trustProse />}
      {reject.isError && (
        <DecisionRefusal error={reject.error} trustProse={false} />
      )}
    </Row>
  );
}
