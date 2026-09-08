import { useState } from "react";
import { isApiRefusal } from "../data/client";
import {
  TRIGGER_SOURCES,
  useCreateTrigger,
  useDeleteTrigger,
  useSetTriggerEnabled,
  useTriggerNext,
  type TeamTrigger,
  type TeamView,
  type TriggerRequest,
} from "../data/teams";
import { Badge, Button, ConfirmButton, ErrorNote, Panel, RefusalNote, RelativeTime } from "../ui";
import { daemonProse } from "./prose";

/**
 * `Routines` — the rules that start this department without anybody asking.
 *
 * **There is no Edit, and there cannot be.** The núcleo mounts `POST`,
 * `DELETE` and `/enable` on `/team-triggers` and nothing else — no `PUT`, no
 * `PATCH` — so an Edit button would answer 405. What this offers instead is
 * **Duplicate**: it fills the write-a-rule form with the rule's own values, so
 * changing one is write-the-new-one-then-delete-the-old-one, done in the order
 * that never leaves the department with no rule at all.
 *
 * The one interlock on this tab is **arming a rule for a department with no
 * budget ceiling**. That is the single control in this whole pillar that can
 * spend without bound — a clock that starts a task that has no limit — and it
 * is therefore the only one that goes through `ConfirmButton`. Everything else
 * stays plain, per the standing rule that the interlock is for destructive
 * writes, plus this one.
 */

export interface RoutinesProps {
  team: TeamView;
  /** Already filtered to this department by the bench. */
  rules: TeamTrigger[];
}

/** The write-a-rule form's fields, which `Duplicate` fills wholesale. */
interface RuleDraft {
  name: string;
  source: (typeof TRIGGER_SOURCES)[number];
  cron: string;
  timezone: string;
  fromTeam: string;
  emailClass: string;
  request: string;
}

function emptyDraft(): RuleDraft {
  return { name: "", source: "cron", cron: "", timezone: "", fromTeam: "", emailClass: "", request: "" };
}

/** A rule, as a draft of another one. The name is marked so two are never confused. */
function draftFrom(rule: TeamTrigger): RuleDraft {
  return {
    name: `${rule.name} (copy)`,
    source: (TRIGGER_SOURCES as readonly string[]).includes(rule.source)
      ? (rule.source as (typeof TRIGGER_SOURCES)[number])
      : "cron",
    cron: rule.cron ?? "",
    timezone: rule.timezone ?? "",
    fromTeam: rule.from_team ?? "",
    emailClass: rule.email_class ?? "",
    request: rule.request,
  };
}

export function Routines({ team, rules }: RoutinesProps) {
  const [draft, setDraft] = useState<RuleDraft>(emptyDraft);

  return (
    <div className="teams-routines">
      <Panel title="Rules">
        <p className="teams-note">
          A rule cannot be edited — the núcleo has no route for it. Duplicate one to write a
          variant, then delete the original.
        </p>
        {rules.length === 0 && <p className="teams-empty">no rule is written for this team.</p>}
        {rules.length > 0 && (
          <ul className="teams-rules" aria-label="Rules">
            {rules.map((rule) => (
              <RuleRow
                key={rule.id}
                rule={rule}
                noCeiling={team.budget_usd === null}
                onDuplicate={() => setDraft(draftFrom(rule))}
              />
            ))}
          </ul>
        )}
      </Panel>

      <Panel title="Write a rule">
        <NewRuleForm teamId={team.id} draft={draft} onChange={setDraft} onWritten={() => setDraft(emptyDraft())} />
      </Panel>
    </div>
  );
}

function RuleRow({
  rule,
  noCeiling,
  onDuplicate,
}: {
  rule: TeamTrigger;
  noCeiling: boolean;
  onDuplicate: () => void;
}) {
  const setEnabled = useSetTriggerEnabled();
  const del = useDeleteTrigger();
  // `enabled` arrives 0 or 1 on the wire, never a real boolean.
  const armed = rule.enabled !== 0;

  return (
    <li className="teams-rule">
      <div className="teams-rule-head">
        <span className="teams-rule-name">{rule.name}</span>
        <Badge tone={armed ? "active" : "off"}>{armed ? "armed" : "disarmed"}</Badge>
        <span className="teams-rule-source">{rule.source}</span>
        {rule.cron !== null && <code className="teams-rule-cron">{rule.cron}</code>}
        {rule.timezone !== null && <span className="teams-rule-zone">{rule.timezone}</span>}
        {rule.from_team !== null && <span className="teams-rule-zone">after {rule.from_team}</span>}
        {rule.email_class !== null && <span className="teams-rule-zone">on {rule.email_class}</span>}
      </div>

      <p className="teams-rule-what">{rule.request}</p>
      <NextLine id={rule.id} />

      <div className="teams-rule-controls">
        {armed ? (
          // Disarming is always plain and never asks.
          <Button onClick={() => setEnabled.mutate({ id: rule.id, enabled: false })} disabled={setEnabled.isPending}>
            Disarm
          </Button>
        ) : noCeiling ? (
          // An armed rule on a department with no ceiling is a clock that can
          // spend without limit — the one place in this design where being
          // wrong costs money without bound.
          <ConfirmButton
            label="Arm with no ceiling"
            confirmLabel="Arm it anyway"
            variant="approve"
            intent="go"
            disabled={setEnabled.isPending}
            onConfirm={() => setEnabled.mutate({ id: rule.id, enabled: true })}
          />
        ) : (
          <Button
            intent="go"
            onClick={() => setEnabled.mutate({ id: rule.id, enabled: true })}
            disabled={setEnabled.isPending}
          >
            Arm
          </Button>
        )}
        {/* Duplicate, never Edit: `DELETE` is the only other verb this route has. */}
        <Button onClick={onDuplicate}>Duplicate</Button>
        <ConfirmButton
          label="Delete"
          confirmLabel="Delete this rule"
          variant="danger"
          intent="stop"
          disabled={del.isPending}
          onConfirm={() => del.mutate(rule.id)}
        />
      </div>
    </li>
  );
}

/**
 * When one rule fires next, with its own `useTriggerNext(id)` — the row-scoped
 * hook pattern.
 *
 * A rule that does not fire on a clock answers 200 with
 * `"this rule does not fire on a clock"`, and a bad cron answers 200 with the
 * daemon's parse message. Neither is an error state and neither is drawn as one.
 */
function NextLine({ id }: { id: number }) {
  const next = useTriggerNext(id);
  if (next.data === undefined) return null;
  if (next.data.error !== null) return <p className="teams-rule-next">{next.data.error}</p>;
  if (next.data.next !== null) {
    return (
      <p className="teams-rule-next">
        next: <RelativeTime at={next.data.next} />
      </p>
    );
  }
  return null;
}

/**
 * Write a rule — and the same form `Duplicate` fills.
 *
 * Controlled from above rather than holding its own state, which is what makes
 * Duplicate possible at all: the parent owns the draft, so a click on a row can
 * put a rule's values into it.
 */
function NewRuleForm({
  teamId,
  draft,
  onChange,
  onWritten,
}: {
  teamId: string;
  draft: RuleDraft;
  onChange: (draft: RuleDraft) => void;
  onWritten: () => void;
}) {
  const create = useCreateTrigger();
  const valid =
    draft.name.trim() !== "" && draft.request.trim() !== "" && (draft.source !== "cron" || draft.cron.trim() !== "");

  function submit() {
    if (!valid || create.isPending) return;
    const body: TriggerRequest = {
      team_id: teamId,
      name: draft.name.trim(),
      source: draft.source,
      cron: draft.source === "cron" ? draft.cron.trim() : null,
      timezone: draft.source === "cron" && draft.timezone.trim() !== "" ? draft.timezone.trim() : null,
      from_team: draft.source === "team_finished" && draft.fromTeam.trim() !== "" ? draft.fromTeam.trim() : null,
      email_class:
        draft.source === "email_triaged" && draft.emailClass.trim() !== "" ? draft.emailClass.trim() : null,
      request: draft.request.trim(),
    };
    create.mutate(body, { onSuccess: onWritten });
  }

  return (
    <form
      className="teams-form"
      onSubmit={(event) => {
        event.preventDefault();
        submit();
      }}
    >
      <p className="teams-note">Writing never arms — arm the rule once it is written.</p>
      <label className="teams-field">
        <span className="teams-label">Name</span>
        <input
          className="teams-input"
          value={draft.name}
          onChange={(event) => onChange({ ...draft, name: event.target.value })}
        />
      </label>
      <label className="teams-field">
        <span className="teams-label">Source</span>
        <select
          className="teams-select"
          value={draft.source}
          onChange={(event) =>
            onChange({ ...draft, source: event.target.value as (typeof TRIGGER_SOURCES)[number] })
          }
        >
          {TRIGGER_SOURCES.map((kind) => (
            <option key={kind} value={kind}>
              {kind}
            </option>
          ))}
        </select>
      </label>
      {draft.source === "cron" && (
        <>
          <label className="teams-field">
            <span className="teams-label">Cron</span>
            <input
              className="teams-input"
              value={draft.cron}
              onChange={(event) => onChange({ ...draft, cron: event.target.value })}
            />
          </label>
          <label className="teams-field">
            <span className="teams-label">Timezone</span>
            <input
              className="teams-input"
              value={draft.timezone}
              onChange={(event) => onChange({ ...draft, timezone: event.target.value })}
            />
          </label>
        </>
      )}
      {draft.source === "team_finished" && (
        <label className="teams-field">
          <span className="teams-label">From team</span>
          <input
            className="teams-input"
            value={draft.fromTeam}
            onChange={(event) => onChange({ ...draft, fromTeam: event.target.value })}
          />
        </label>
      )}
      {draft.source === "email_triaged" && (
        <label className="teams-field">
          <span className="teams-label">Email class</span>
          <input
            className="teams-input"
            value={draft.emailClass}
            onChange={(event) => onChange({ ...draft, emailClass: event.target.value })}
          />
        </label>
      )}
      <label className="teams-field">
        <span className="teams-label">Request</span>
        <textarea
          className="teams-textarea"
          rows={3}
          value={draft.request}
          onChange={(event) => onChange({ ...draft, request: event.target.value })}
        />
      </label>
      <div className="teams-actions">
        <Button type="submit" intent="go" disabled={!valid || create.isPending}>
          Write rule
        </Button>
      </div>
      {create.isError && <CreateRefusal error={create.error} />}
    </form>
  );
}

function CreateRefusal({ error }: { error: unknown }) {
  if (isApiRefusal(error)) return <RefusalNote refusal={error} sentences={daemonProse(error)} />;
  return <ErrorNote>the núcleo did not answer — this rule was not written</ErrorNote>;
}
