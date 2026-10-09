import { useId, useState } from "react";
import { useMutation } from "@tanstack/react-query";
import { apiFetch } from "../data/client";
import { Badge, Button, ErrorNote, Field, Quiet } from "../ui";
import "./loadout-preview.css";

type PreviewBox = "team" | "job_node";

/** What `POST /loadout/preview` answers: exactly what a spawn in that box would hand the agent. */
export interface LoadoutPreviewAnswer {
  agent_id: string;
  team_id: string | null;
  box: PreviewBox;
  memory: string | null;
  block: string;
  tools: { name: string; origin: "base" | "agent" | "team" }[];
  refs: {
    owner_kind: "agent" | "team";
    path: string;
    kind: string;
    note: string | null;
    state: "active" | "missing" | "refused";
  }[];
  add_dirs: string[];
}

const ORIGIN: Record<LoadoutPreviewAnswer["tools"][number]["origin"], string> = {
  base: "box base",
  agent: "approved for the agent",
  team: "approved for the team",
};

const BOXES: { value: PreviewBox; label: string }[] = [
  { value: "team", label: "Team" },
  { value: "job_node", label: "Job node" },
];

/**
 * What this agent would receive at spawn for a sample task: the memory block,
 * the effective tools with where each came from, and the context files.
 * Read-only — the núcleo resolves it the way a spawn does and records nothing.
 */
export function LoadoutPreview({ agentId }: { agentId: string }) {
  const [task, setTask] = useState("");
  const [box, setBox] = useState<PreviewBox>("team");
  const boxName = useId();
  const preview = useMutation({
    mutationFn: () =>
      apiFetch<LoadoutPreviewAnswer>("/loadout/preview", {
        method: "POST",
        body: JSON.stringify({ agent_id: agentId, box, task }),
      }),
  });

  return (
    <section className="learned-group loadout-preview" aria-label="Loadout preview">
      <h3>Loadout preview</h3>
      <form
        className="loadout-preview-form"
        onSubmit={(event) => {
          event.preventDefault();
          preview.mutate();
        }}
      >
        <Field label="Sample task">
          <textarea rows={2} value={task} onChange={(event) => setTask(event.target.value)} />
        </Field>
        <fieldset className="loadout-preview-box">
          <legend>Runs in</legend>
          {BOXES.map((option) => (
            <label key={option.value}>
              <input
                type="radio"
                name={boxName}
                value={option.value}
                checked={box === option.value}
                onChange={() => setBox(option.value)}
              />
              {option.label}
            </label>
          ))}
        </fieldset>
        <Button type="submit" disabled={preview.isPending}>
          Preview
        </Button>
      </form>
      {preview.isError && <ErrorNote>The núcleo did not answer the preview.</ErrorNote>}
      {preview.data !== undefined && <PreviewResult answer={preview.data} />}
    </section>
  );
}

function PreviewResult({ answer }: { answer: LoadoutPreviewAnswer }) {
  const refs = (answer.refs ?? []).filter((ref) => ref.state !== "refused");
  const tools = answer.tools ?? [];
  return (
    <div className="loadout-preview-result">
      <section aria-label="Memory it would receive">
        <h4>Memory</h4>
        {answer.memory ? (
          <pre className="loadout-preview-block">{answer.memory}</pre>
        ) : (
          <Quiet says="No memory would be shown for this task." />
        )}
      </section>
      <section aria-label="Tools it would hold">
        <h4>Tools</h4>
        {tools.length === 0 ? (
          <Quiet says="No tools: this agent would run without any." />
        ) : (
          <ul className="loadout-preview-list">
            {tools.map((tool) => (
              <li key={tool.name}>
                <code>{tool.name}</code>
                <Badge tone={tool.origin === "base" ? "off" : "info"}>{ORIGIN[tool.origin]}</Badge>
              </li>
            ))}
          </ul>
        )}
      </section>
      <section aria-label="Context it would be offered">
        <h4>Context</h4>
        {refs.length === 0 ? (
          <Quiet says="No context files would be offered." />
        ) : (
          <ul className="loadout-preview-list">
            {refs.map((ref) => (
              <li key={`${ref.owner_kind}:${ref.path}`}>
                <code>{ref.path}</code>
                <span className="loadout-preview-muted">
                  {ref.kind}, {ref.owner_kind}
                  {ref.note ? ` — ${ref.note}` : ""}
                </span>
                {ref.state === "missing" && <Badge tone="danger">missing</Badge>}
              </li>
            ))}
          </ul>
        )}
      </section>
      {answer.block && (
        <details>
          <summary>Exact text appended to the prompt</summary>
          <pre className="loadout-preview-block">{answer.block}</pre>
        </details>
      )}
    </div>
  );
}
