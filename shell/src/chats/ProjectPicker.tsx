import { useId, useState, type ReactNode } from "react";
import { Button, Modal } from "../ui";
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
  const choices: Choice[] = rooted.map((project) => ({
    key: `project:${project.project_id}`,
    label: project.project_id,
    path: project.project_root,
  }));
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
              {root ??
                (home.isPending
                  ? "reading where NucleOS lives…"
                  : "the núcleo did not say where it lives")}
            </span>
          </label>
        )}
      </div>
      {refusal}
    </Modal>
  );
}
