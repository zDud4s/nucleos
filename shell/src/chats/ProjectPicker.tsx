import { useId, useState, type ReactNode } from "react";
import { Check, FolderRoot } from "lucide-react";
import { Button, Modal, Quiet } from "../ui";
import { DAEMON_NAME } from "../copy";
import { useHome, useProjects } from "../data/system";

/** One row in the picker: a project, or Root. */
interface Choice {
  key: string;
  label: string;
  path: string;
}

export interface ProjectPickerProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /** Called with the absolute folder the chosen row stands for. */
  onChoose: (cwd: string) => void;
  /** While the choice is being written: the confirm button waits. */
  pending: boolean;
  /** What the daemon said when it refused the last choice, already worded. */
  refusal?: ReactNode;
  /** Whether a message is waiting on this choice — said, because it is why the dialog is here. */
  holding: boolean;
}

/**
 * Where a conversation runs, chosen before it says anything.
 *
 * A choice among the NucleOS projects — the daemon's roster, the same list the Projects page
 * draws — plus Root, the folder NucleOS itself lives in (`GET /home`, whatever the daemon was started from). Not a free-text folder: a conversation is about a
 * project this app already looks after, and a path typed by hand was one more way to point it
 * somewhere nothing else in the app knows about.
 *
 * Dismissing it chooses nothing and loses nothing: a held message stays held, and the line the
 * conversation shows in the dialog's place opens it again.
 */
export function ProjectPicker({
  open,
  onOpenChange,
  onChoose,
  pending,
  refusal,
  holding,
}: ProjectPickerProps) {
  const projects = useProjects();
  const home = useHome();
  const group = useId();
  const [chosen, setChosen] = useState<string | null>(null);

  const rooted = (projects.data ?? []).filter(
    (project): project is typeof project & { project_root: string } =>
      project.project_root !== null,
  );
  // By name, ignoring case: the roster's own order put every lower-case name after every capital.
  const choices: Choice[] = rooted
    .map((project) => ({
      key: `project:${project.project_id}`,
      label: project.project_id,
      path: project.project_root,
    }))
    .sort((a, b) => a.label.localeCompare(b.label, undefined, { sensitivity: "base" }));
  // Whatever the daemon says and nothing else: until it has, Root is not offered a path.
  const root = home.data?.root ?? null;
  const picked =
    chosen === "root"
      ? root
      : (choices.find((choice) => choice.key === chosen)?.path ?? null);

  return (
    <Modal
      open={open}
      onOpenChange={onOpenChange}
      title="Choose where this conversation runs"
      size="md"
      description={
        holding
          ? "Your message is held and goes out as soon as this is chosen. Without a project a conversation can talk, but cannot open a file, run a command, or change anything on this machine."
          : "Nothing is sent until this is chosen. Without a project a conversation can talk, but cannot open a file, run a command, or change anything on this machine."
      }
      footer={
        <>
          <Button type="button" onClick={() => onOpenChange(false)}>
            Not now
          </Button>
          <Button
            type="button"
            variant="approve"
            intent="go"
            disabled={pending || picked === null}
            onClick={() => {
              if (picked !== null) onChoose(picked);
            }}
          >
            Use this
          </Button>
        </>
      }
    >
      <div className="chats-picker" role="radiogroup" aria-label="Where this conversation runs">
        {projects.isPending && <p className="chats-loading">reading the projects…</p>}
        {projects.isError && projects.data === undefined && (
          <Quiet compact says={`The ${DAEMON_NAME} did not list its projects`} />
        )}
        {projects.data !== undefined && choices.length === 0 && (
          <Quiet compact says="No project on the roster has a folder yet. Add one on the Projects page." />
        )}
        {projects.data !== undefined && (
          /* First, and set apart: it is not a project but the folder NucleOS itself runs from. */
          <PickerRow
            group={group}
            checked={chosen === "root"}
            disabled={root === null}
            onPick={() => setChosen("root")}
            onUse={() => {
              if (root !== null && !pending) onChoose(root);
            }}
            name="Root"
            icon
            path={
              root ??
              (home.isPending
                ? "reading where NucleOS lives…"
                : "The background service did not say where it lives")
            }
          />
        )}
        {choices.length > 0 && <div className="chats-picker-rule" aria-hidden="true" />}
        {choices.map((choice) => (
          <PickerRow
            key={choice.key}
            group={group}
            checked={chosen === choice.key}
            onPick={() => setChosen(choice.key)}
            onUse={() => {
              if (!pending) onChoose(choice.path);
            }}
            name={choice.label}
            path={choice.path}
          />
        ))}
      </div>
      {refusal}
    </Modal>
  );
}

/**
 * A path as this machine writes it. The roster stores some with forward slashes and some with
 * back, and a list that mixes the two reads as two kinds of thing. Display only: what is chosen is
 * the path exactly as the daemon sent it.
 */
export function displayPath(path: string): string {
  return /^[A-Za-z]:[\\/]/.test(path) ? path.replace(/\//g, "\\") : path;
}

/**
 * One choice: a real radio, hidden but focusable, inside a row that is the target. A double click
 * chooses and confirms at once, which is what a list of places is usually asked for twice over.
 */
function PickerRow({
  group,
  checked,
  disabled = false,
  onPick,
  onUse,
  name,
  path,
  icon = false,
}: {
  group: string;
  checked: boolean;
  disabled?: boolean;
  onPick: () => void;
  onUse: () => void;
  name: string;
  path: string;
  icon?: boolean;
}) {
  return (
    <label
      className="chats-picker-row"
      data-disabled={disabled ? "" : undefined}
      onDoubleClick={disabled ? undefined : onUse}
    >
      <input
        type="radio"
        className="sr-only"
        name={group}
        checked={checked}
        disabled={disabled}
        onChange={onPick}
      />
      {icon && <FolderRoot className="chats-picker-icon" aria-hidden="true" />}
      <span className="chats-picker-text">
        <span className="chats-picker-name">{name}</span>
        <span className="chats-picker-path" title={path}>
          {displayPath(path)}
        </span>
      </span>
      <Check className="chats-picker-check" aria-hidden="true" />
    </label>
  );
}
