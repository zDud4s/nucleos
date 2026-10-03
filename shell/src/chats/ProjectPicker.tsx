import { useId, useState, type ReactNode } from "react";
import { Button, Modal } from "../ui";
import { useProjects } from "../data/system";

/**
 * The folder every rostered project lives under, or null when there is no sensible one.
 *
 * This is what "Root" means in the picker: not one project, but the place the projects sit side by
 * side — `C:/Projects` for `C:/Projects/nucleos` and `C:/Projects/site`. A conversation rooted
 * there can look across all of them, which is what somebody asks for when the question is about
 * more than one project or about none in particular.
 *
 * Derived from the roster rather than configured, because the roster is the only place this app
 * already knows where projects are. The longest common ancestor of every project root; when that
 * ancestor is itself one of the projects (one project, or one nested in another), its parent, so
 * Root is never just another name for a project already in the list.
 *
 * Null when the answer would be a bare filesystem root (`C:/`, `/`) or the roots share nothing
 * (two drives): handing a conversation tools over a whole drive is not what the word was asked to
 * mean, and the option is then offered disabled rather than guessed.
 */
export function projectsRoot(roots: string[]): string | null {
  const split = roots
    .map((root) => root.replace(/\\/g, "/").replace(/\/+$/, ""))
    .filter((root) => root !== "")
    .map((root) => root.split("/"));
  if (split.length === 0) return null;

  let common = split[0];
  for (const parts of split.slice(1)) {
    let at = 0;
    while (
      at < common.length &&
      at < parts.length &&
      common[at].toLowerCase() === parts[at].toLowerCase()
    ) {
      at += 1;
    }
    common = common.slice(0, at);
  }
  // A project as the common ancestor is not a place ABOVE the projects; step out of it.
  if (split.some((parts) => parts.length === common.length)) {
    common = common.slice(0, -1);
  }
  // `["C:"]` and `[""]` (a POSIX `/`) are whole filesystems, and `[]` is nothing shared at all.
  const meaningful = common.filter((part) => part !== "");
  if (meaningful.length < 2) return null;
  return common.join("/");
}

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
 * draws — plus Root (see `projectsRoot`). Not a free-text folder: a conversation is about a
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
  const group = useId();
  const [chosen, setChosen] = useState<string | null>(null);

  const rooted = (projects.data ?? []).filter(
    (project): project is typeof project & { project_root: string } =>
      project.project_root !== null,
  );
  const choices: Choice[] = rooted.map((project) => ({
    key: `project:${project.project_id}`,
    label: project.project_id,
    path: project.project_root,
  }));
  const root = projectsRoot(rooted.map((project) => project.project_root));
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
          <p className="chats-picker-none">the núcleo did not answer with its projects</p>
        )}
        {projects.data !== undefined && choices.length === 0 && (
          <p className="chats-picker-none">
            no project on the roster has a folder yet — add one on the Projects page
          </p>
        )}
        {choices.map((choice) => (
          <label key={choice.key} className="chats-picker-row">
            <input
              type="radio"
              name={group}
              checked={chosen === choice.key}
              onChange={() => setChosen(choice.key)}
            />
            <span className="chats-picker-name">{choice.label}</span>
            <span className="chats-picker-path">{choice.path}</span>
          </label>
        ))}
        {projects.data !== undefined && (
          <label className="chats-picker-row">
            <input
              type="radio"
              name={group}
              checked={chosen === "root"}
              disabled={root === null}
              onChange={() => setChosen("root")}
            />
            <span className="chats-picker-name">Root</span>
            <span className="chats-picker-path">
              {root ?? "no folder holds all the projects"}
            </span>
          </label>
        )}
      </div>
      {refusal}
    </Modal>
  );
}
