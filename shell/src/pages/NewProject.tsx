// §spec workspace-de-projeto

import { useEffect, useId, useRef, useState, type ReactNode, type RefObject } from "react";
import { Link, useNavigate, useSearch } from "@tanstack/react-router";
import { isTauri } from "@tauri-apps/api/core";
import { FolderOpen } from "lucide-react";
import { open as pickFolder } from "@tauri-apps/plugin-dialog";
import { isApiRefusal } from "../data/client";
import {
  suggestedId,
  useAdoptWorkflow,
  useDetect,
  type Detected,
  type Harness,
} from "../data/detect";
import { useOnboard } from "../data/onboarding";
import { useDeclareProjectCommand } from "../data/project-commands";
import { useSetWipLimit } from "../data/projects";
import { useSetProjectMode } from "../data/autopilot";
import { useKillSwitch, useProjects } from "../data/system";
import { useInstallWorkflow, useWorkflowLibrary } from "../data/workflows";
import { MODE_MEANING, MODE_REFUSAL_PROSE } from "../lib/mode";
import { Roster } from "./Roster";
import "./new-project.css";
import {
  Button,
  ConflictNote,
  ErrorNote,
  Field,
  Modal,
  RefusalNote,
  StateBadge,
} from "../ui";

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
 * nobody had yet decided to trust. Promotion is earned against evidence, with the same switch on
 * the project's own page and on the Autopilot page.
 *
 * `?path=` prefills the folder and reads it at once. Reading writes nothing, and a page that sent
 * somebody here with a folder already in mind (the roster, offering to add one back) should not
 * make them type it again. The route declares no validator for it, so it is read unvalidated here
 * and anything that is not a non-empty string is the same as not asking.
 *
 * **A dialog over the roster, not a page of its own** (owner's call, 2026-10-09). The route stays —
 * links, `?path=` and the sidebar all still point at `/projects/new` — but what it draws is the
 * roster with this wizard lifted over it on the shared `Modal`'s blurred scrim. Closing it, by
 * Escape, the scrim or the X, goes back to `/projects`, and the wizard's progress goes with it.
 */

export function NewProject() {
  const navigate = useNavigate();
  const asked = pathFrom(useSearch({ strict: false }) as Record<string, unknown>);
  const [typed, setTyped] = useState(asked ?? "");
  const [looking, setLooking] = useState<string | null>(asked);
  const helpId = useId();

  const found = useDetect(looking);

  /*
    The native folder picker. Always offered, so the control is where a person looks for it, but it
    only answers inside the app: a browser (the preview) has no file dialog that hands back an
    absolute path, so there the button says so instead. A folder chosen in the app is read at once,
    as `?path=` is: the person already said which folder, and a click on "look" would be ceremony.
  */
  const [browseNote, setBrowseNote] = useState<string | null>(null);
  async function browse() {
    setBrowseNote(null);
    if (!isTauri()) {
      setBrowseNote("the folder picker only opens in the desktop app — here, type the path");
      return;
    }
    try {
      const chosen = await pickFolder({
        directory: true,
        multiple: false,
        title: "Choose the project's folder",
        defaultPath: typed.trim() === "" ? undefined : typed.trim(),
      });
      if (typeof chosen !== "string" || chosen === "") return;
      setTyped(chosen);
      setLooking(chosen);
    } catch {
      setBrowseNote("the folder picker did not open — type the path instead");
    }
  }
  const projects = useProjects();
  const count = projects.data?.length;

  return (
    <>
      <Roster />
      <Modal
        open
        onOpenChange={(open) => {
          if (!open) void navigate({ to: "/projects" });
        }}
        title="Add a project"
        description={
          count === undefined
            ? undefined
            : `${count === 0 ? "No project" : `${count} ${count === 1 ? "project" : "projects"}`} on the roster — a new one always starts in shadow.`
        }
        size="lg"
      >
        <div className="flex flex-col gap-5">
          <Step n={1} label="The folder">
            {/* The field and its two answers on one line: Browse beside the path it fills, and Look —
                the step's one action — last and solid. The step heading already says "The folder",
                so the field's own label is for a screen reader only. */}
            <form
              className="flex flex-wrap items-center gap-2"
              onSubmit={(event) => {
                event.preventDefault();
                setLooking(typed.trim() === "" ? null : typed.trim());
              }}
            >
              <div className="min-w-56 flex-1">
                <Field label="Folder" labelHidden>
                  <input
                    placeholder="C:/Projects/something"
                    value={typed}
                    spellCheck={false}
                    aria-describedby={helpId}
                    onChange={(event) => setTyped(event.target.value)}
                    className="w-full font-mono"
                  />
                </Field>
              </div>
              <Button type="button" onClick={() => void browse()}>
                <FolderOpen size={14} aria-hidden="true" />
                Browse…
              </Button>
              {/* `aria-busy` and a word, not a spinner: `useDetect` runs three git commands against a
                  disk, and on a slow one a button that said nothing read as a button that did not
                  work. The name only changes while it is busy, so "Look" is what a voice finds. */}
              <Button type="submit" intent="go" aria-busy={found.isFetching}>
                {found.isFetching ? "Looking…" : "Look"}
              </Button>
            </form>
            <p id={helpId} className="text-xs text-text-muted">
              An absolute path on this machine. Nothing is written until the last step.
            </p>
            {browseNote !== null && <ErrorNote>{browseNote}</ErrorNote>}
            {found.isError ? <WhyNot error={found.error} /> : null}
          </Step>

          {found.data === undefined ? null : (
            <Found
              // Keyed by the folder, so a second look starts steps two and three over rather than
              // carrying the first folder's ticks and id into the second.
              key={found.data.root}
              found={found.data}
              // The path was edited after it was read. Steps two and three still describe the old
              // folder, and adding would register THAT one — so they stay visible, and say so, and
              // the button waits for another look.
              folderEdited={looking !== null && typed.trim() !== looking}
              onDone={(projectId) =>
                void navigate({
                  to: "/projects/$projectId/$view",
                  params: { projectId, view: "state" },
                })
              }
            />
          )}
        </div>
      </Modal>
    </>
  );
}

/** `?path=`, if it is really there. Empty and absent are the same claim. */
function pathFrom(search: Record<string, unknown>): string | null {
  const raw = search.path;
  return typeof raw === "string" && raw.trim() !== "" ? raw.trim() : null;
}

/**
 * The daemon's three answers about a folder, in the page's own words.
 *
 * `RefusalNote` for the three, because each is the daemon answering — a path that is not there is a
 * fact about the path, not a fault — and `ErrorNote` for the one case where nothing answered.
 */
const FOLDER_SENTENCES: Record<string, string> = {
  no_such_folder: "there is nothing at that path.",
  not_a_folder: "that path is a file, not a folder.",
  not_absolute: "that has to be an absolute path — there is no folder for it to be relative to yet.",
};

function WhyNot({ error }: { error: unknown }) {
  if (!isApiRefusal(error)) {
    return <ErrorNote>the núcleo did not answer about that folder.</ErrorNote>;
  }
  return <RefusalNote refusal={error} sentences={FOLDER_SENTENCES} />;
}

/** How many steps this page has. Named once so a step cannot say "1." with no denominator. */
const STEPS = 3;


function Step({
  n,
  label,
  headingRef,
  children,
}: {
  n: number;
  label: string;
  /** Where focus goes when the step appears — see `Found`. */
  headingRef?: RefObject<HTMLHeadingElement | null>;
  children: ReactNode;
}) {
  return (
    <section
      aria-label={label}
      className={`flex flex-col gap-3 ${n === 1 ? "" : "border-t border-border pt-5"}`}
    >
      <h2
        ref={headingRef}
        tabIndex={headingRef === undefined ? undefined : -1}
        className="np-step-title flex items-center gap-2 font-display text-sm font-semibold text-text"
      >
        <span
          aria-hidden="true"
          className="grid size-5 shrink-0 place-items-center rounded-full border border-border-strong font-mono text-xs text-text-muted"
        >
          {n}
        </span>
        <span className="sr-only">
          {n} of {STEPS}.{" "}
        </span>
        {label}
      </h2>
      {children}
    </section>
  );
}

/* --------------------------------------------------- steps two and three -- */

/** The four writes, in the order `finish` makes them. */
type Stage = "register" | "workflow" | "commands" | "ceiling";

/** One write this page will make: what it is called on screen, and the call itself. */
interface Planned {
  key: string;
  stage: Stage;
  says: ReactNode;
  /** The same thing as a clause, for "stopped at …". */
  doing: ReactNode;
  run: () => Promise<unknown>;
}

/** How far a `finish` got. `reached` is the index of the write in flight, or the one that failed. */
interface Ran {
  plan: Planned[];
  projectId: string;
  reached: number;
  error: unknown;
  failed: boolean;
}

/**
 * A project id in the shape `suggestedId` produces, and the test is that function itself: an id is
 * in shape exactly when suggesting from it changes nothing. One rule and not a second regex beside
 * it, which is how the suggestion and the check would come to disagree.
 *
 * It goes into every URL this project has, which is why spaces, capitals and slashes are refused
 * here rather than by a daemon answer at the bottom of the page.
 */
function idInShape(id: string): boolean {
  return id !== "" && suggestedId(id) === id;
}

/** The nearest id in shape. Slashes become dashes first — `suggestedId` would keep only the last segment. */
function shapedId(id: string): string {
  return suggestedId(id.replace(/[\\/]+/g, "-"));
}

function Found({
  found,
  folderEdited,
  onDone,
}: {
  found: Detected;
  folderEdited: boolean;
  onDone: (projectId: string) => void;
}) {
  const [projectId, setProjectId] = useState(suggestedId(found.root));
  const [adopting, setAdopting] = useState<string | null>(found.harnesses[0]?.path ?? null);
  const [taking, setTaking] = useState<Set<string>>(new Set());
  const [installing, setInstalling] = useState<string | null>(null);
  /** `null` is no ceiling — the brake off, and not a ceiling of zero. Same meaning as Settings. */
  const [wip, setWip] = useState<number | null>(2);
  const [ran, setRan] = useState<Ran | null>(null);
  const [busy, setBusy] = useState(false);
  /**
   * Whether onboarding was skipped because the stop was engaged, set inside `finish`'s register
   * step and read once the whole plan lands. A `ref` and not `useState` there: `finish` needs the
   * answer synchronously, in the same pass, to decide whether to navigate — a state update would
   * not be visible until the next render, by which point the decision is already made.
   */
  const onboardWaitingRef = useRef(false);
  const [onboardWaiting, setOnboardWaiting] = useState(false);
  /** The gate command to confirm, starting from what the núcleo proposes. Blank is none. */
  const [gate, setGate] = useState(found.gate?.command ?? "");

  const library = useWorkflowLibrary();
  const projects = useProjects();
  const kill = useKillSwitch();
  const setMode = useSetProjectMode();
  const onboard = useOnboard();
  const setWipLimit = useSetWipLimit();
  const declare = useDeclareProjectCommand();
  const adopt = useAdoptWorkflow();
  const install = useInstallWorkflow();

  const idHelpId = useId();
  const reasonId = useId();
  const ceilingLabelId = useId();
  const ceilingHelpId = useId();
  const harnessGroup = useId();

  /**
   * Step two arrived after a keypress in step one, below the focus. A screen reader heard nothing
   * and a keyboard would have had to Tab through the folder field again to find it — so focus goes
   * to the heading, which announces where it landed. Once per folder: `Found` is keyed by it.
   */
  const foundHeading = useRef<HTMLHeadingElement>(null);
  useEffect(() => {
    foundHeading.current?.focus();
  }, []);

  /** Something was written: from here on, a second press would repeat writes rather than retry. */
  const registered = ran !== null && (ran.reached > 0 || !ran.failed);
  const taken = found.taken_by !== null;
  const shaped = idInShape(projectId);
  const clash =
    !registered && (projects.data?.some((row) => row.project_id === projectId) ?? false);
  const workflowChosen = adopting !== null || installing !== null;
  /**
   * The emergency stop pre-emptively refuses a workflow write and nothing else of the four —
   * registering, the commands and the ceiling are database rows that start nothing. So the stop is
   * not a reason to refuse the project, only the workflow, and it is said here, before the first
   * write, rather than found out after the project row already exists.
   *
   * Onboarding, inside the register step, is the exception that proves this: it now consults the
   * same switch, because it is a governance write like the ones `POST /write` already guards. It is
   * not blocked here, because its refusal does not need to be — `finish`'s register step catches it
   * and skips onboarding rather than the whole step, so the stop still never refuses the project.
   */
  const stopHolds = kill.data?.engaged === true && workflowChosen;

  /** Why the button is not pressable, most fundamental first. One reason, said beside it. */
  const blocker: string | null = taken
    ? `already registered as ${found.taken_by ?? ""}`
    : folderEdited
      ? "the folder was edited after it was read — look again"
      : !shaped
        ? "the project id is not in shape yet"
        : clash
          ? `there is already a project called ${projectId}`
          : stopHolds
            ? "the kill switch would refuse the workflow"
            : null;

  /**
   * Everything, in one order, and the order is the design.
   *
   * The project row first, because everything after it is addressed by the id and by the folder the
   * row records. Then the workflow, then the commands, then the ceiling. Each step is a route that
   * already existed and already refuses for its own reasons.
   *
   * The list is built before anything is sent and then frozen into `ran`, for two reasons. It is
   * what the page shows before the button — a receipt of the writes, read before signing it — and
   * it is what a failure is reported against: the writes that landed, the one that stopped, and the
   * ones never tried, so a project half-configured is never half-configured with nothing saying
   * which half.
   */
  const plan: Planned[] = [
    {
      key: "register",
      stage: "register",
      says: (
        <>
          register <span className="font-mono">{projectId}</span> at{" "}
          <span className="font-mono">{found.root}</span>, in{" "}
          <StateBadge domain="autopilot" state="shadow" />, onboarded with{" "}
          {gate.trim() === "" ? (
            "no gate command"
          ) : (
            <>
              the gate <span className="font-mono">{gate.trim()}</span>
            </>
          )}
        </>
      ),
      doing: "registering the project",
      // Onboarding first, in the same step: the mode door refuses a project nobody onboarded, and
      // the two together are what "registering" means. Onboarding is idempotent, so a retry after a
      // refused registration repeats nothing that matters. **Shadow, always.** §9. A project that
      // started acting on its own the moment it was added would be one nobody had decided to trust.
      //
      // A `kill_switch` refusal from onboarding is not this step's failure: registering a project
      // has never been something the stop refuses (see `stopHolds` above, for the workflow write),
      // and onboarding is a governance write like the ones the stop already guards, so it now
      // consults the same switch. Skip it rather than fail the whole step — the project still gets
      // registered, in shadow, and onboarding can be finished from its Settings once released.
      run: async () => {
        try {
          await onboard.mutateAsync({ projectId, projectRoot: found.root, gateCommand: gate });
        } catch (error) {
          if (isApiRefusal(error) && error.code === "kill_switch") {
            onboardWaitingRef.current = true;
          } else {
            throw error;
          }
        }
        return setMode.mutateAsync({
          project_id: projectId,
          mode: "shadow",
          project_root: found.root,
        });
      },
    },
    ...(adopting !== null
      ? [
          {
            key: "workflow",
            stage: "workflow" as const,
            says: (
              <>
                adopt <span className="font-mono">{adopting}</span> as its way of working
              </>
            ),
            doing: (
              <>
                adopting <span className="font-mono">{adopting}</span>
              </>
            ),
            run: () => adopt.mutateAsync({ projectId, name: "harness", path: adopting }),
          },
        ]
      : installing !== null
        ? [
            {
              key: "workflow",
              stage: "workflow" as const,
              says: (
                <>
                  install <span className="font-mono">{installing.replace("@", " ")}</span> from the
                  library
                </>
              ),
              doing: (
                <>
                  installing <span className="font-mono">{installing.replace("@", " ")}</span>
                </>
              ),
              run: () => {
                const [name, version] = installing.split("@");
                return install.mutateAsync({ projectId, name, version });
              },
            },
          ]
        : []),
    ...found.commands
      .filter((row) => taking.has(row.name))
      .map((suggestion) => ({
        key: `command:${suggestion.name}`,
        stage: "commands" as const,
        says: (
          <>
            declare <span className="font-mono">{suggestion.name}</span>, not as a gate
          </>
        ),
        doing: (
          <>
            declaring <span className="font-mono">{suggestion.name}</span>
          </>
        ),
        run: () =>
          declare.mutateAsync({
            projectId,
            name: suggestion.name,
            command: suggestion.command,
            // Nothing arrives as a gate. Marking one is a claim about what its result MEANS, and a
            // wizard that guessed would put a claim in the bar that nobody made.
            is_gate: false,
            runnable_by: "person",
          }),
      })),
    {
      key: "ceiling",
      stage: "ceiling",
      says: wip === null ? "no open-proposal ceiling" : `an open-proposal ceiling of ${wip}`,
      doing: "setting the open-proposal ceiling",
      run: () => setWipLimit.mutateAsync({ projectId, limit: wip }),
    },
  ];

  async function finish() {
    const frozen = plan;
    const id = projectId;
    setBusy(true);
    onboardWaitingRef.current = false;
    setRan({ plan: frozen, projectId: id, reached: 0, error: null, failed: false });
    let at = 0;
    try {
      for (; at < frozen.length; at += 1) {
        setRan({ plan: frozen, projectId: id, reached: at, error: null, failed: false });
        await frozen[at].run();
      }
      setRan({ plan: frozen, projectId: id, reached: frozen.length, error: null, failed: false });
      // Onboarding waiting on the stop is not a reason to leave this page unfinished — the project
      // is registered — but it is a reason to stay rather than navigate straight to a page whose
      // Onboard panel would say the same thing to someone who never read it here.
      if (onboardWaitingRef.current) {
        setOnboardWaiting(true);
      } else {
        onDone(id);
      }
    } catch (error) {
      setRan({ plan: frozen, projectId: id, reached: at, error, failed: true });
    } finally {
      setBusy(false);
    }
  }

  /**
   * Past the first write, every control here is settled: changing a tick now would describe a
   * project that no longer matches what was sent. A failure BEFORE the first write leaves nothing
   * behind, so that one unlocks and the button stays.
   */
  const locked = busy || registered;
  const shown = ran?.plan ?? plan;

  return (
    <fieldset disabled={locked} className="m-0 flex min-w-0 flex-col gap-5 border-0 p-0">
      <Step n={2} label="What is already there" headingRef={foundHeading}>
        <dl className="np-repo">
          <Row label="Folder" value={found.root} mono />
          <Row
            label="Git"
            value={
              found.is_git
                ? [found.branch, found.remote].filter(Boolean).join(" · ") || "a repository"
                : "not a repository — which is allowed, but it can only ever run in shadow"
            }
          />
          {found.head === null ? null : <Row label="Last commit" value={found.head} mono />}
        </dl>

        {taken ? (
          <ConflictNote>
            This folder is already registered as{" "}
            <span className="font-mono">{found.taken_by}</span>. Adding it again under a second
            name would give one folder two sets of rules and two open-proposal ceilings, and
            neither would be wrong on its own.
          </ConflictNote>
        ) : null}

        <Harnesses
          group={harnessGroup}
          harnesses={found.harnesses}
          chosen={adopting}
          onChoose={(path) => {
            setAdopting(path);
            if (path !== null) setInstalling(null);
          }}
        />

        <div className="np-group">
          <h3 className="np-group-title">
            Commands
            {found.commands.length === 0 ? null : <span className="np-group-n">{found.commands.length}</span>}
          </h3>
          {found.commands.length === 0 ? (
            <p className="np-hint">
              None found in a <span className="font-mono">package.json</span>, a{" "}
              <span className="font-mono">Makefile</span> or a Cargo config. They can be declared on
              the project's own page.
            </p>
          ) : (
            <>
              <p className="np-hint">
                Tick the ones worth a button on the project's page. Nothing unticked is saved, and none
                of them becomes the gate — that is the field in step 3.
              </p>
              <div className="np-list">
                {found.commands.map((suggestion) => (
                  <label key={suggestion.name} className="np-command">
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
                    <span className="np-command-name">{suggestion.name}</span>
                    <span className="np-command-line">{suggestion.command}</span>
                    <span className="np-command-source">{suggestion.source}</span>
                  </label>
                ))}
              </div>
              {found.commands_omitted > 0 ? (
                <p className="np-hint">
                  {found.commands_omitted} more were found and are not listed — the rest can be
                  declared on the project's page.
                </p>
              ) : null}
            </>
          )}
        </div>
      </Step>

      <Step n={3} label="How much it may do">
        <div className="np-fields">
          <Field
            label="Project id"
            helper={
              shaped ? (
                "Lowercase letters, digits and dashes — it becomes the project's address."
              ) : (
                <>
                  {projectId === "" ? "an id is needed" : "not an id yet"} — lowercase letters,
                  digits and dashes, no dash at either end.
                  {shapedId(projectId) === "" ? null : (
                    <>
                      {" "}
                      <Button variant="quiet" onClick={() => setProjectId(shapedId(projectId))}>
                        use {shapedId(projectId)}
                      </Button>
                    </>
                  )}
                </>
              )
            }
          >
            <input
              value={projectId}
              spellCheck={false}
              aria-invalid={!shaped || clash}
              aria-describedby={clash ? idHelpId : undefined}
              onChange={(event) => setProjectId(event.target.value)}
              className="w-full font-mono"
            />
          </Field>

          {/* The one claim about what a result MEANS that this page makes, and it makes it only in
              words somebody confirmed: the field starts from the núcleo's proposal and is stored
              as typed. Blank is a real answer — no gate — and can be set later with the rules. */}
          <Field
            label="Gate command"
            helper={
              found.gate === null
                ? "Its exit code says the project is green. Nothing here suggests one; blank is none."
                : `Its exit code says the project is green — proposed from ${found.gate.source}; blank is none.`
            }
          >
            <input
              value={gate}
              spellCheck={false}
              placeholder="none"
              onChange={(event) => setGate(event.target.value)}
              className="w-full font-mono"
            />
          </Field>
        </div>

        {/* An id another project already has. `set_project_mode` is an upsert — the route is how
            a project is registered at all — so this would not be refused: it would re-point that
            project at this folder. The one mistake here the daemon cannot catch. */}
        {clash ? (
          <div id={idHelpId}>
            <ConflictNote>
              There is already a project called <span className="font-mono">{projectId}</span>.
              Adding this folder under that id would move that project here rather than add a new
              one — choose another id.
            </ConflictNote>
          </div>
        ) : null}

        {found.harnesses.length === 0 && library.data !== undefined && library.data.length > 0 ? (
          <div className="np-fields">
            <Field label="Workflow">
              <select
                value={installing ?? ""}
                onChange={(event) => setInstalling(event.target.value === "" ? null : event.target.value)}
                className="w-full"
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
            </Field>
          </div>
        ) : null}

        {/* The same name, the same control and the same "no ceiling" as the Settings block this
            number is found under a second later. It was "wip ceiling" here with no way to switch
            it off, and a reader met one setting under two names with two different ranges. */}
        <div className="np-setting" role="group" aria-labelledby={ceilingLabelId} aria-describedby={ceilingHelpId}>
          <div className="np-setting-text">
            <span id={ceilingLabelId} className="np-setting-label">
              Open-proposal ceiling
            </span>
            <span id={ceilingHelpId} className="np-hint">
              How many proposals may wait on your review before new work is held. Two keeps a new
              project's first ones few enough to read.
            </span>
          </div>
          <div className="np-stepper">
            <Button
              aria-label="Lower the ceiling"
              disabled={wip === null || wip <= 1}
              onClick={() => setWip(wip === null ? null : wip - 1)}
            >
              −
            </Button>
            <span className="np-stepper-value">
              {/* "off", as Settings says it: a brake switched off is not a missing reading. */}
              {wip === null ? "off" : wip}
            </span>
            <Button aria-label="Raise the ceiling" onClick={() => setWip(wip === null ? 1 : wip + 1)}>
              +
            </Button>
            <Button variant="quiet" disabled={wip === null} onClick={() => setWip(null)}>
              no ceiling
            </Button>
          </div>
        </div>

        <p className="np-callout">
          <StateBadge domain="autopilot" state="shadow" />
          <span>
            It starts in shadow: {MODE_MEANING.shadow}. Nothing here can let it act — that is unlocked
            later, on the project's page or on Autopilot, once the evidence earns it.
          </span>
        </p>

        {stopHolds && !registered ? (
          <div className="flex flex-col items-start gap-2">
            <ConflictNote>
              The kill switch is engaged, and while it is the núcleo refuses to record a workflow
              for any project. Adding this now would register it and then stop. The project, its
              commands and its ceiling can be added without the workflow, and the workflow adopted
              from the project's page once the stop is released.
            </ConflictNote>
            <Button
              onClick={() => {
                setAdopting(null);
                setInstalling(null);
              }}
            >
              leave the workflow for later
            </Button>
          </div>
        ) : null}

        <Receipt plan={shown} ran={ran} busy={busy} />

        {registered && ran?.failed === true ? (
          <Stopped ran={ran} />
        ) : registered && onboardWaiting ? (
          <OnboardWaiting projectId={ran?.projectId ?? projectId} />
        ) : (
          <>
            {/* A failure on the very first write: nothing was written, so the button below stays
                and pressing it again is a retry rather than a repeat. */}
            {ran?.failed === true ? <WriteRefused stage={ran.plan[ran.reached]} error={ran.error} /> : null}
            <div className="np-actions">
              {blocker === null ? null : (
                <span id={reasonId} className="np-actions-why">
                  {blocker}
                </span>
              )}
              <Button
                intent="go"
                disabled={busy || blocker !== null}
                aria-describedby={blocker === null ? undefined : reasonId}
                onClick={() => void finish()}
              >
                {busy ? "Adding…" : "Add it, in shadow"}
              </Button>
            </div>
          </>
        )}
      </Step>
    </fieldset>
  );
}

/**
 * The writes, as a list, before and after.
 *
 * Before the button, it is what pressing it will do — read before signing. After a press, each line
 * says what became of it, in words rather than glyphs: `done`, the one in flight, the one that
 * `stopped here`, and `not tried`. The list does not change shape between the two, so a failure
 * points at a line the reader has already read.
 */
function Receipt({ plan, ran, busy }: { plan: Planned[]; ran: Ran | null; busy: boolean }) {
  function status(index: number): string | null {
    if (ran === null) return null;
    if (index < ran.reached) return "done";
    if (index > ran.reached) return ran.failed ? "not tried" : null;
    if (ran.failed) return "stopped here";
    return busy ? "writing…" : null;
  }

  return (
    <div className="np-receipt">
      <p className="np-receipt-title">Adding it will:</p>
      <ol className="np-receipt-list">
        {plan.map((item, index) => {
          const said = status(index);
          return (
            <li key={item.key} className="np-receipt-item" data-status={said ?? undefined}>
              <span>{item.says}</span>
              {said === null ? null : <span className="np-receipt-status">{said}</span>}
            </li>
          );
        })}
      </ol>
    </div>
  );
}

/** Why one write did not land. The mode route's own sentences for the first; the shared floor after. */
function WriteRefused({ stage, error }: { stage: Planned | undefined; error: unknown }) {
  if (!isApiRefusal(error)) {
    return (
      <ErrorNote>
        the núcleo did not answer while {stage?.doing ?? "the project was being set up"}. Whether that
        write landed is not known — the project's page will show what it has.
      </ErrorNote>
    );
  }
  return (
    <RefusalNote
      refusal={error}
      sentences={stage?.stage === "register" ? MODE_REFUSAL_PROSE : undefined}
    />
  );
}

/**
 * The receipt of a finish that stopped after the project existed.
 *
 * What a bug report needs, because the reader of this is the one who fixes the bug: the id it is
 * registered under, the write that stopped, and why. The button is gone rather than left inviting a
 * second press — that would repeat the registration and the writes that already landed — and the
 * way forward is the project's own page, where each of the remaining settings has its control.
 */
function Stopped({ ran }: { ran: Ran }) {
  const stage = ran.plan[ran.reached];
  return (
    <div className="flex flex-col items-start gap-2">
      <p className="max-w-(--measure) text-sm text-text">
        Registered as <span className="font-mono">{ran.projectId}</span> in shadow, and stopped at{" "}
        {stage?.doing ?? "the next step"}. Nothing after it was tried.
      </p>
      <WriteRefused stage={stage} error={ran.error} />
      <Link
        className="text-sm"
        to="/projects/$projectId/$view"
        params={{ projectId: ran.projectId, view: "state" }}
      >
        Finish setting up {ran.projectId} on its page
      </Link>
    </div>
  );
}

/**
 * The receipt of a finish that landed in full, with onboarding held back by the stop.
 *
 * Not a failure — nothing here `stopped here`, and there is no retry to invite — so it does not
 * reuse {@link Stopped}. It exists so the wizard says this once, in its own words, rather than
 * navigating straight to the project's page and leaving that page's `Onboard` panel as the only
 * place it is ever said.
 */
function OnboardWaiting({ projectId }: { projectId: string }) {
  return (
    <div className="flex flex-col items-start gap-2">
      <p className="max-w-(--measure) text-sm text-text">
        Registered as <span className="font-mono">{projectId}</span> in shadow. The stop is
        engaged, so onboarding — the confirmed gate and the classifier hook — is waiting until it
        is released; it can be finished from the project's Settings once it is.
      </p>
      <Link
        className="text-sm"
        to="/projects/$projectId/$view"
        params={{ projectId, view: "state" }}
      >
        Go to {projectId}
      </Link>
    </div>
  );
}

function Row({ label, value, mono = false }: { label: string; value: string; mono?: boolean }) {
  return (
    <div className="np-repo-fact">
      <dt>{label}</dt>
      <dd className={mono ? "font-mono" : undefined}>{value}</dd>
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
  group,
  harnesses,
  chosen,
  onChoose,
}: {
  /** The radio group's name, unique per mount — a fixed `name` would join two wizards' radios. */
  group: string;
  harnesses: Harness[];
  chosen: string | null;
  onChoose: (path: string | null) => void;
}) {
  if (harnesses.length === 0) return null;

  return (
    <div className="np-group">
      <h3 className="np-group-title">Way of working</h3>
      <p className="np-hint">
        Adopting it records that it is here — the folder is not copied, not rewritten, and nothing is
        added to it. It receives no updates, because there is no library it came from.
      </p>
      <div className="np-options">
        {harnesses.map((harness) => (
          <label key={harness.path} className="np-option">
            <input
              type="radio"
              name={group}
              checked={chosen === harness.path}
              onChange={() => onChoose(harness.path)}
            />
            <span className="np-option-name font-mono">{harness.path}</span>
            <span className="np-option-meta">
              <span>
                {harness.files} {harness.files === 1 ? "file" : "files"}
              </span>
              {" · "}
              {harness.what}
            </span>
          </label>
        ))}
        <label className="np-option">
          <input type="radio" name={group} checked={chosen === null} onChange={() => onChoose(null)} />
          <span className="np-option-name">adopt none of them</span>
        </label>
      </div>
    </div>
  );
}
