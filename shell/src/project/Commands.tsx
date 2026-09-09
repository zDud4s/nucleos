// §spec workspace-de-projeto
import { useEffect, useState } from "react";
import { isApiRefusal } from "../data/client";
import {
  outcomeSentence,
  outcomeTone,
  useDeclareProjectCommand,
  useForgetProjectCommand,
  useProjectCommands,
  useRunProjectCommand,
  type ProjectCommand,
} from "../data/project-commands";
import { Button, Quiet } from "../ui";
import {
  CommandDialog,
  CommandEmpty,
  CommandGroup,
  CommandInput,
  CommandItem,
  CommandList,
} from "../ui/vendor/command";

/**
 * What this project can be asked to do to itself.
 *
 * **This is the section a page like this usually dies of**, and §4.6 names the death: a row of
 * buttons at the foot of the screen with no owner, which is how every dashboard ends up with
 * fourteen of them. Three rules keep it away, and all three are about *placement* rather than about
 * restraint:
 *
 * 1. **A command that acts on a thing lives in that thing.** Reviewing a run is an action about
 *    that slot, so it is drawn inside the slot — it is not here and never will be.
 * 2. **The bar holds the gates.** A gate's last verdict is a fact you want without asking: *is this
 *    green*. That is what earns a permanent place at the foot of the page.
 * 3. **Everything else is in the palette.** A verb you go looking for by name does not need to be
 *    on the screen while you are not looking for it. ⌘K, the same door `Chats` opens.
 *
 * Nothing lands in the bar by accumulating there. A command is in it because somebody marked it a
 * gate, which is a claim about what its result means.
 */

export interface CommandsProps {
  projectId: string;
}

export function Commands({ projectId }: CommandsProps) {
  const commands = useProjectCommands(projectId);
  const run = useRunProjectCommand();
  const [paletteOpen, setPaletteOpen] = useState(false);
  const [managing, setManaging] = useState(false);

  /**
   * Ctrl+K, and Cmd+K for the same fingers on a Mac keyboard — the binding `Chats` already uses,
   * because two palettes with two shortcuts would be two things to remember.
   *
   * On `window`, and `preventDefault` because Ctrl+K is a browser shortcut the webview would
   * otherwise act on as well.
   */
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key.toLowerCase() !== "k" || !(event.ctrlKey || event.metaKey)) return;
      event.preventDefault();
      setPaletteOpen((open) => !open);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  if (commands.data === undefined) {
    return <p className="text-sm text-text-faint">Reading this project's commands…</p>;
  }

  const rows = commands.data;
  const gates = rows.filter((row) => row.is_gate);
  const refused = run.isError && isApiRefusal(run.error) ? run.error : null;

  const declare = (
    <Button variant="quiet" onClick={() => setManaging(!managing)}>
      {managing ? "done" : "declare a command"}
    </Button>
  );

  /*
    The palette is offered from both shapes below and written once. Closed, it puts nothing in the
    document at all, which is what lets the quiet line be the section's only child.
  */
  const palette = (
    <CommandPalette
      rows={rows}
      projectId={projectId}
      open={paletteOpen}
      onOpenChange={setPaletteOpen}
      onRun={(id) => run.mutate({ projectId, id })}
    />
  );

  /*
    Nothing declared, and nobody halfway through declaring one. The sentence about declaration
    rather than detection is kept and not cut — "nothing declared yet" on its own reads as a list
    that failed to load, and this one never fills itself — but it is worth a click rather than a
    paragraph of every visit. The one gesture that would fill the section stays in front of you.
  */
  if (rows.length === 0 && !managing) {
    return (
      <>
        <Quiet says="none declared" action={declare}>
          A command is a name, something to run, and whether its result is a verdict on the
          project — declaration rather than detection, so a list nobody agreed to cannot appear on
          its own.
        </Quiet>
        {palette}
      </>
    );
  }

  return (
    <div className="flex flex-col gap-2">
      {rows.length === 0 ? null : (
        <>
          <div className="flex flex-wrap items-center gap-2">
            {gates.length === 0 ? (
              /*
                No gate is a state, not an empty bar. A project with commands and none of them
                marked a verdict has nothing to say about being green, and saying nothing is more
                honest than drawing a bar of verbs that were never claimed to mean anything.
              */
              <p className="text-sm text-text-faint">
                No command here is marked a gate, so nothing on this page claims to say whether{" "}
                {projectId} is green.
              </p>
            ) : (
              gates.map((row) => (
                <GateButton
                  key={row.id}
                  row={row}
                  disabled={run.isPending}
                  onRun={() => run.mutate({ projectId, id: row.id })}
                />
              ))
            )}
            <button
              type="button"
              onClick={() => setPaletteOpen(true)}
              className="rounded-md border border-border px-2 py-1 text-xs text-text-faint hover:border-border-strong"
            >
              all {rows.length} · ⌘K
            </button>
          </div>

          {refused !== null ? (
            <p className="text-xs text-tone-danger-fg">{REFUSALS[refused.code] ?? refused.detail}</p>
          ) : null}
        </>
      )}

      <div>{declare}</div>
      {managing ? <Manage projectId={projectId} rows={rows} /> : null}

      {palette}
    </div>
  );
}

/**
 * Declaring one, and forgetting one.
 *
 * Behind a disclosure rather than on the page, because this is the rarest thing anybody does here —
 * you declare `gate` once and press it for a year — and a form permanently occupying the foot of
 * the page would be the drawer by another route.
 *
 * **A form and not a text box**, which is the same call `Settings` makes about the same layer: the
 * app knows the shape, so there is no way to declare a command with no name, and `is_gate` is a
 * checkbox rather than a word somebody has to spell. What the app does NOT know is whether the
 * words will spawn — an unbalanced quote, a folder that is not there — and the núcleo answers that
 * with the reason, which is what `detail` carries.
 */
function Manage({ projectId, rows }: { projectId: string; rows: ProjectCommand[] }) {
  const declare = useDeclareProjectCommand();
  const forget = useForgetProjectCommand();
  const [name, setName] = useState("");
  const [command, setCommand] = useState("");
  const [cwd, setCwd] = useState("");
  const [isGate, setIsGate] = useState(false);
  const [agentMayRun, setAgentMayRun] = useState(false);

  const refused = declare.isError && isApiRefusal(declare.error) ? declare.error : null;
  const ready = name.trim() !== "" && command.trim() !== "";

  return (
    <div className="flex flex-col gap-3 rounded-lg border border-border bg-surface p-4">
      {rows.length > 0 ? (
        <ul className="flex flex-col gap-1">
          {rows.map((row) => (
            <li key={row.id} className="flex flex-wrap items-baseline gap-2 text-sm">
              <span className="text-text">{row.name}</span>
              <span className="font-mono text-xs text-text-faint">{row.command}</span>
              {row.is_gate ? <span className="text-xs text-text-muted">gate</span> : null}
              {row.runnable_by === "agent" ? (
                <span className="text-xs text-tone-shadow-fg">agents may run this</span>
              ) : null}
              {/*
                A workflow's command has no forget button, and that is the overlay rather than a
                missing feature: it belongs to the bundle, and the way to be rid of it is to
                override it with one of this project's own. Offering a button that refuses would be
                a worse answer than offering none.
              */}
              {row.source === "project" ? (
                <span className="ml-auto">
                  <Button variant="quiet" onClick={() => forget.mutate({ projectId, id: row.id })}>
                    forget
                  </Button>
                </span>
              ) : (
                <span className="ml-auto text-xs text-text-faint">the workflow's</span>
              )}
            </li>
          ))}
        </ul>
      ) : null}

      <div className="flex flex-col gap-2">
        <div className="flex flex-wrap gap-2">
          <input
            aria-label="Command name"
            placeholder="gate"
            value={name}
            spellCheck={false}
            onChange={(event) => setName(event.target.value)}
            className="w-32 rounded-md border border-border bg-surface-sunken px-2 py-1 text-sm text-text"
          />
          <input
            aria-label="What it runs"
            placeholder="cargo test"
            value={command}
            spellCheck={false}
            onChange={(event) => setCommand(event.target.value)}
            className="min-w-48 flex-1 rounded-md border border-border bg-surface-sunken px-2 py-1 font-mono text-sm text-text"
          />
          <input
            aria-label="Folder it runs in"
            placeholder="the project root"
            value={cwd}
            spellCheck={false}
            onChange={(event) => setCwd(event.target.value)}
            className="w-40 rounded-md border border-border bg-surface-sunken px-2 py-1 font-mono text-sm text-text"
          />
        </div>

        <div className="flex flex-wrap items-center gap-4 text-xs text-text-muted">
          <label className="flex items-center gap-1.5">
            <input
              type="checkbox"
              checked={isGate}
              onChange={(event) => setIsGate(event.target.checked)}
            />
            its result says whether this project is green
          </label>
          <label className="flex items-center gap-1.5">
            <input
              type="checkbox"
              checked={agentMayRun}
              onChange={(event) => setAgentMayRun(event.target.checked)}
            />
            an agent may run it on its own
          </label>
        </div>

        <div className="flex items-center gap-2">
          <button
            type="button"
            disabled={!ready || declare.isPending}
            onClick={() =>
              declare.mutate(
                {
                  projectId,
                  name: name.trim(),
                  command,
                  cwd: cwd.trim() === "" ? null : cwd.trim(),
                  is_gate: isGate,
                  runnable_by: agentMayRun ? "agent" : "person",
                },
                {
                  // Cleared only on success, so a refused declaration leaves what was typed in
                  // front of the person who typed it.
                  onSuccess: () => {
                    setName("");
                    setCommand("");
                    setCwd("");
                    setIsGate(false);
                    setAgentMayRun(false);
                  },
                },
              )
            }
            className="rounded-md border border-border px-3 py-1.5 text-xs text-text enabled:hover:border-border-strong disabled:opacity-40"
          >
            {declare.isPending ? "declaring…" : "declare"}
          </button>
          <span className="text-xs text-text-faint">
            A name already declared is replaced rather than duplicated.
          </span>
        </div>

        {refused !== null ? (
          <p className="rounded-md border border-tone-danger-border bg-tone-danger-bg p-2 text-xs text-text-muted">
            {/* The núcleo's own words for `invalid`, which name the part that was wrong. */}
            {DECLARE_REFUSALS[refused.code] ?? refused.detail}
          </p>
        ) : null}
      </div>
    </div>
  );
}

const DECLARE_REFUSALS: Record<string, string> = {
  cwd_missing: "that folder is not in this project.",
  cwd_not_a_folder: "that path names a file, not a folder.",
  cwd_unsafe: "the folder must be inside the project.",
  no_project_root: "the núcleo has no folder recorded for this project.",
  internal: "the núcleo hit an error of its own while saving it.",
};

/** What each refusal means, in the page's words. */
const REFUSALS: Record<string, string> = {
  kill_switch: "the emergency stop is engaged, so nothing here starts a process.",
  already_running: "that one is already going. One at a time, per command.",
  no_such_command: "the núcleo has no such command for this project any more.",
  no_project_root: "the núcleo has no folder recorded for this project.",
  cwd_missing: "the folder that command runs in is not there.",
  cwd_not_a_folder: "what that command runs in is a file, not a folder.",
  person_only: "that command is marked for a person, and this is not one.",
  internal: "the núcleo hit an error of its own trying to start it.",
};

/**
 * One gate, with what it last said.
 *
 * The verdict is a dot **and** a sentence in the title, never a colour alone: `failed` and
 * `errored` are two facts that send somebody to two different places, and a reader who cannot
 * distinguish red from amber would otherwise be told the tests broke when the measurement did.
 */
function GateButton({
  row,
  disabled,
  onRun,
}: {
  row: ProjectCommand;
  disabled: boolean;
  onRun: () => void;
}) {
  const tone = outcomeTone(row.last);
  const said = outcomeSentence(row.last);
  const running = row.last?.outcome === "running";

  return (
    <button
      type="button"
      disabled={disabled || running}
      onClick={onRun}
      title={`${row.command} — ${said}`}
      aria-label={`${row.name}, ${said}`}
      className="flex items-center gap-2 rounded-md border border-border bg-surface px-3 py-1.5 text-sm text-text enabled:hover:border-border-strong disabled:opacity-60"
    >
      <span
        aria-hidden
        className="h-1.5 w-1.5 rounded-pill"
        style={{
          // No tone is the absence of a verdict, drawn as an outline rather than as a colour. A
          // grey dot would be a fifth state that reads like a sixth opinion.
          background: tone === null ? "transparent" : `var(--tone-${tone}-fg)`,
          boxShadow: tone === null ? "inset 0 0 0 1px var(--border-strong)" : undefined,
        }}
      />
      {row.name}
      <span className="text-xs text-text-faint">{running ? "running…" : said}</span>
    </button>
  );
}

/**
 * Every command, searchable.
 *
 * The palette is where the list is allowed to be long, because nothing in it is on screen until
 * somebody asks for it by name. That is the whole reason the bar can stay short.
 */
function CommandPalette({
  rows,
  projectId,
  open,
  onOpenChange,
  onRun,
}: {
  rows: ProjectCommand[];
  projectId: string;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onRun: (id: number) => void;
}) {
  return (
    <CommandDialog
      open={open}
      onOpenChange={onOpenChange}
      title={`Run something in ${projectId}`}
      description="Type to narrow the list. Enter runs the one highlighted."
      /* Escape closes it. A palette is dismissed rather than closed, and the corner X is clutter
         that also has to be styled — the same call `Chats` made. */
      showCloseButton={false}
    >
      <CommandInput placeholder="Run…" />
      <CommandList>
        <CommandEmpty>No command matches that.</CommandEmpty>
        <CommandGroup>
          {rows.map((row) => (
            <CommandItem
              key={row.id}
              // Searchable by what it RUNS as well as by its name: somebody looking for the clippy
              // one may not remember what it was called.
              value={`${row.name} ${row.command}`}
              disabled={row.last?.outcome === "running"}
              onSelect={() => {
                onOpenChange(false);
                onRun(row.id);
              }}
            >
              <span className="text-text">{row.name}</span>
              <span className="ml-2 font-mono text-xs text-text-faint">{row.command}</span>
              <span className="ml-auto text-xs text-text-faint">{outcomeSentence(row.last)}</span>
            </CommandItem>
          ))}
        </CommandGroup>
      </CommandList>
    </CommandDialog>
  );
}
