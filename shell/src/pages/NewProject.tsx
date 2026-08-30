// §spec workspace-de-projeto

import { useState } from "react";
import { useNavigate } from "@tanstack/react-router";
import { isApiRefusal } from "../data/client";
import {
  suggestedId,
  useAdoptWorkflow,
  useDetect,
  type Detected,
  type Harness,
} from "../data/detect";
import { useDeclareProjectCommand } from "../data/project-commands";
import { useSetWipLimit } from "../data/projects";
import { useSetProjectMode } from "../data/autopilot";
import { useInstallWorkflow, useWorkflowLibrary } from "../data/workflows";
import { PageHeader } from "../ui";

/**
 * Adding a project: point at a folder, see what is already in it, choose how much it may do.
 *
 * §9's three steps, and the middle one is what decides whether this app is hostile to what exists.
 * A project worth adding has been developed for a while — it has a history, commands its people
 * type, and often a written-down way of working. **This repository has one, in `.ai/`, and it built
 * NucleOS.** An app that asked for all of that to be described again in its own forms before it
 * would admit the project exists would be asking for a day's work to write down a thing that is
 * sitting right there.
 *
 * So the núcleo reads and this **proposes**. Nothing is stored that nobody ticked, which is the same
 * rule the command registry settled: a list that changed by itself when a `package.json` was edited
 * would be noise, and there would be nowhere in it to mark which one is the gate.
 *
 * **It finishes in shadow, always.** §9 says so and the reason is the whole of the design's
 * restraint: a project that started acting on its own the moment it was added would be a project
 * nobody had yet decided to trust. Promotion is earned on the Autopilot page, against evidence.
 */

export function NewProject() {
  const navigate = useNavigate();
  const [typed, setTyped] = useState("");
  const [looking, setLooking] = useState<string | null>(null);

  const found = useDetect(looking);

  return (
    <>
      <PageHeader
        title="Add a project"
        headline="point at a folder, see what is already in it, then decide how much it may do"
      />

      <div className="flex max-w-3xl flex-col gap-6">
        <Step n={1} label="The folder">
          <form
            className="flex flex-wrap gap-2"
            onSubmit={(event) => {
              event.preventDefault();
              setLooking(typed.trim() === "" ? null : typed.trim());
            }}
          >
            <input
              aria-label="Folder"
              placeholder="C:/Projects/something"
              value={typed}
              spellCheck={false}
              onChange={(event) => setTyped(event.target.value)}
              className="min-w-64 flex-1 rounded-md border border-border bg-surface-sunken px-2 py-1.5 font-mono text-sm text-text"
            />
            <button
              type="submit"
              className="rounded-md border border-border px-3 py-1.5 text-sm text-text hover:border-border-strong"
            >
              look
            </button>
          </form>
          <p className="mt-2 text-xs text-text-faint">
            An absolute path on this machine. Nothing is written until the last step.
          </p>
          {found.isError ? <WhyNot error={found.error} /> : null}
        </Step>

        {found.data === undefined ? null : (
          <Found found={found.data} onDone={(id) => void navigate({ to: `/projects/${id}/estado` })} />
        )}
      </div>
    </>
  );
}

/** Why the folder could not be read, in the daemon's own three answers. */
function WhyNot({ error }: { error: unknown }) {
  const text = !isApiRefusal(error)
    ? "the núcleo did not answer about that folder."
    : error.code === "no_such_folder"
      ? "there is nothing at that path."
      : error.code === "not_a_folder"
        ? "that path is a file, not a folder."
        : error.code === "not_absolute"
          ? "that has to be an absolute path — there is no folder for it to be relative to yet."
          : (error.detail ?? "the núcleo refused to read that folder.");
  return (
    <p className="mt-2 rounded-md border border-tone-danger-border bg-tone-danger-bg p-2 text-xs text-text-muted">
      {text}
    </p>
  );
}

function Step({ n, label, children }: { n: number; label: string; children: React.ReactNode }) {
  return (
    <section aria-label={label} className="flex flex-col gap-2">
      <h2 className="font-display text-xs font-medium uppercase tracking-wider text-text-faint">
        {n}. {label}
      </h2>
      <div className="rounded-lg border border-border bg-surface p-4">{children}</div>
    </section>
  );
}

/* --------------------------------------------------- steps two and three -- */

function Found({ found, onDone }: { found: Detected; onDone: (projectId: string) => void }) {
  const [projectId, setProjectId] = useState(suggestedId(found.root));
  const [adopting, setAdopting] = useState<string | null>(found.harnesses[0]?.path ?? null);
  const [taking, setTaking] = useState<Set<string>>(new Set());
  const [installing, setInstalling] = useState<string | null>(null);
  const [wip, setWip] = useState(2);

  const library = useWorkflowLibrary();
  const setMode = useSetProjectMode();
  const setWipLimit = useSetWipLimit();
  const declare = useDeclareProjectCommand();
  const adopt = useAdoptWorkflow();
  const install = useInstallWorkflow();
  const [failed, setFailed] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const taken = found.taken_by !== null;

  /**
   * Everything, in one order, and the order is the design.
   *
   * The project row first, because everything after it is addressed by the id and by the folder the
   * row records. Then the workflow, then the commands, then the ceiling. Each step is a route that
   * already existed and already refuses for its own reasons; a failure stops and says which, rather
   * than carrying on and leaving a project half-configured with nothing saying so.
   */
  async function finish() {
    setBusy(true);
    setFailed(null);
    try {
      // **Shadow, always.** §9. A project that started acting on its own the moment it was added
      // would be one nobody had decided to trust yet.
      await setMode.mutateAsync({
        project_id: projectId,
        mode: "shadow",
        project_root: found.root,
      });

      if (adopting !== null) {
        await adopt.mutateAsync({ projectId, name: "harness", path: adopting });
      } else if (installing !== null) {
        const [name, version] = installing.split("@");
        await install.mutateAsync({ projectId, name, version });
      }

      for (const suggestion of found.commands.filter((row) => taking.has(row.name))) {
        await declare.mutateAsync({
          projectId,
          name: suggestion.name,
          command: suggestion.command,
          // Nothing arrives as a gate. Marking one is a claim about what its result MEANS, and a
          // wizard that guessed would put a claim in the bar that nobody made.
          is_gate: false,
          runnable_by: "person",
        });
      }

      await setWipLimit.mutateAsync({ projectId, limit: wip });
      onDone(projectId);
    } catch (error) {
      setFailed(
        isApiRefusal(error)
          ? (error.detail ?? error.code)
          : "the núcleo did not answer while the project was being set up.",
      );
    } finally {
      setBusy(false);
    }
  }

  return (
    <>
      <Step n={2} label="What is already there">
        <dl className="flex flex-col gap-1 text-sm">
          <Row label="folder" value={found.root} mono />
          <Row
            label="git"
            value={
              found.is_git
                ? [found.branch, found.remote].filter(Boolean).join(" · ") || "a repository"
                : "not a repository — which is allowed, but it can only ever run in shadow"
            }
          />
          {found.head === null ? null : <Row label="last commit" value={found.head} mono />}
        </dl>

        {taken ? (
          <p className="mt-3 rounded-md border border-tone-paused-border bg-tone-paused-bg p-2 text-xs text-text-muted">
            This folder is already registered as <span className="font-mono">{found.taken_by}</span>.
            Adding it again under a second name would give one folder two sets of rules and two WIP
            ceilings, and neither would be wrong on its own.
          </p>
        ) : null}

        <Harnesses
          harnesses={found.harnesses}
          chosen={adopting}
          onChoose={(path) => {
            setAdopting(path);
            if (path !== null) setInstalling(null);
          }}
        />

        {found.commands.length === 0 ? (
          <p className="mt-4 text-xs text-text-faint">
            No commands found in a <span className="font-mono">package.json</span>, a{" "}
            <span className="font-mono">Makefile</span> or a Cargo config. They can be declared on
            the project's own page.
          </p>
        ) : (
          <div className="mt-4 flex flex-col gap-1.5">
            <p className="text-xs text-text-muted">
              Found in this folder. Tick the ones worth having a button for — nothing is saved that
              is not ticked, and none of them becomes a gate here: saying a command's result decides
              whether the project is green is a claim to make deliberately, on the project's page.
            </p>
            {found.commands.map((suggestion) => (
              <label key={suggestion.name} className="flex flex-wrap items-baseline gap-2 text-sm">
                <input
                  type="checkbox"
                  checked={taking.has(suggestion.name)}
                  onChange={(event) => {
                    const next = new Set(taking);
                    if (event.target.checked) next.add(suggestion.name);
                    else next.delete(suggestion.name);
                    setTaking(next);
                  }}
                />
                <span className="text-text">{suggestion.name}</span>
                <span className="font-mono text-xs text-text-faint">{suggestion.command}</span>
                <span className="text-xs text-text-muted">{suggestion.source}</span>
              </label>
            ))}
            {found.commands_omitted > 0 ? (
              <p className="text-xs text-text-faint">
                {found.commands_omitted} more were found and are not listed — the rest can be
                declared on the project's page.
              </p>
            ) : null}
          </div>
        )}
      </Step>

      <Step n={3} label="How much it may do">
        <div className="flex flex-col gap-3">
          <label className="flex flex-wrap items-center gap-2 text-sm text-text-muted">
            <span className="w-28">called</span>
            <input
              aria-label="Project name"
              value={projectId}
              spellCheck={false}
              onChange={(event) => setProjectId(event.target.value)}
              className="min-w-48 rounded-md border border-border bg-surface-sunken px-2 py-1 font-mono text-sm text-text"
            />
          </label>

          {found.harnesses.length === 0 && library.data !== undefined && library.data.length > 0 ? (
            <label className="flex flex-wrap items-center gap-2 text-sm text-text-muted">
              <span className="w-28">workflow</span>
              <select
                aria-label="Workflow"
                value={installing ?? ""}
                onChange={(event) => setInstalling(event.target.value === "" ? null : event.target.value)}
                className="rounded-md border border-border bg-surface-sunken px-2 py-1 text-sm text-text"
              >
                {/* None is first and is a real answer: a project that develops however whoever is
                    at the keyboard decides is not a project missing something. */}
                <option value="">none</option>
                {library.data.map((bundle) => (
                  <option key={`${bundle.name}@${bundle.version}`} value={`${bundle.name}@${bundle.version}`}>
                    {bundle.name} {bundle.version}
                  </option>
                ))}
              </select>
            </label>
          ) : null}

          <label className="flex flex-wrap items-center gap-2 text-sm text-text-muted">
            <span className="w-28">wip ceiling</span>
            <input
              aria-label="WIP ceiling"
              type="number"
              min={1}
              value={wip}
              onChange={(event) => setWip(Math.max(1, Number(event.target.value) || 1))}
              className="w-20 rounded-md border border-border bg-surface-sunken px-2 py-1 text-sm text-text"
            />
            <span className="text-xs text-text-faint">
              how much unreviewed work this project may be holding at once
            </span>
          </label>

          <p className="text-xs text-text-muted">
            It starts in <span className="text-tone-shadow-fg">shadow</span>: the núcleo watches and
            records what it would have done, and does none of it. Nothing here can put a project
            into active — that is earned on the Autopilot page, against the evidence.
          </p>

          <div className="flex items-center gap-3">
            <button
              type="button"
              disabled={busy || taken || projectId.trim() === ""}
              onClick={() => void finish()}
              className="rounded-md border border-border px-3 py-1.5 text-sm text-text enabled:hover:border-border-strong disabled:opacity-40"
            >
              {busy ? "adding…" : "add it, in shadow"}
            </button>
            {taken ? (
              <span className="text-xs text-text-faint">
                already registered as {found.taken_by}
              </span>
            ) : null}
          </div>

          {failed !== null ? (
            <p className="rounded-md border border-tone-danger-border bg-tone-danger-bg p-2 text-xs text-text-muted">
              {failed}
            </p>
          ) : null}
        </div>
      </Step>
    </>
  );
}

function Row({ label, value, mono = false }: { label: string; value: string; mono?: boolean }) {
  return (
    <div className="flex flex-wrap items-baseline gap-2">
      <dt className="w-24 shrink-0 text-xs uppercase tracking-wide text-text-faint">{label}</dt>
      <dd className={`min-w-0 break-words ${mono ? "font-mono text-xs" : "text-sm"} text-text`}>
        {value}
      </dd>
    </div>
  );
}

/**
 * The way of working this project already has.
 *
 * **Adopting copies nothing and writes nothing into the folder.** That is the whole of §9's second
 * step: the app recognises what is there rather than asking for it to be recreated. What it cannot
 * do is receive updates — there is no library this came from — and the sentence says so, because a
 * pin that quietly never updates is the silence §6.1 spends a section on.
 */
function Harnesses({
  harnesses,
  chosen,
  onChoose,
}: {
  harnesses: Harness[];
  chosen: string | null;
  onChoose: (path: string | null) => void;
}) {
  if (harnesses.length === 0) return null;

  return (
    <div className="mt-4 flex flex-col gap-1.5">
      <p className="text-xs text-text-muted">
        This project already has a way of working written down. Adopting it records that it is here —
        the folder is not copied, not rewritten, and nothing is added to it. It receives no updates,
        because there is no library it came from.
      </p>
      {harnesses.map((harness) => (
        <label key={harness.path} className="flex flex-wrap items-baseline gap-2 text-sm">
          <input
            type="radio"
            name="harness"
            checked={chosen === harness.path}
            onChange={() => onChoose(harness.path)}
          />
          <span className="font-mono text-text">{harness.path}</span>
          <span className="text-xs text-text-faint">
            {harness.files} {harness.files === 1 ? "file" : "files"}
          </span>
          <span className="text-xs text-text-muted">{harness.what}</span>
        </label>
      ))}
      <label className="flex items-baseline gap-2 text-sm">
        <input
          type="radio"
          name="harness"
          checked={chosen === null}
          onChange={() => onChoose(null)}
        />
        <span className="text-text-muted">adopt none of them</span>
      </label>
    </div>
  );
}
