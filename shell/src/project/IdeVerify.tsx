import { isApiRefusal } from "../data/client";
import {
  useSetIdeVerify,
  type IdeVerifyAnswer,
  type IdeVerifyState,
  type ProjectRules,
} from "../data/projects";
import { Button, ConfirmButton, ErrorNote, Panel, RefusalNote } from "../ui";
import { Why } from "./OnItsOwn";
/* The `pj-` family, as in `OnItsOwn`: the panel draws correctly wherever it is mounted. */
import "../pages/projects.css";

/**
 * The owner's IDE verify switch for one project.
 *
 * The state comes from the rules read (`rules.ide_verify`) and from nothing else: there is no
 * optimistic draw, so a refused or lost write leaves the state line saying what the núcleo said.
 * The report of what the reconcile did to each worktree comes from the mutation's own answer.
 * Drawing the panel never posts; only a press does.
 */
export function IdeVerifyPanel({
  projectId,
  rules,
  stale,
}: {
  projectId: string;
  rules: ProjectRules;
  stale: boolean;
}) {
  const setSwitch = useSetIdeVerify();
  const reported = rules.ide_verify;
  const busy = setSwitch.isPending;

  function send(enabled: boolean) {
    setSwitch.mutate({ projectId, enabled });
  }

  return (
    <Panel title="IDE verify">
      <Why lead="Lets an IDE session in this project's worktrees reach the núcleo's verify box.">
        <p>
          Switching it on writes a daemon-owned <code>.mcp.json</code> and an
          approval in <code>.claude/settings.local.json</code> into every IDE
          worktree of the project, hidden from git. Switching it off removes
          exactly that and nothing else.
        </p>
      </Why>

      {reported !== undefined && (
        <p className="pj-wip-state">
          IDE verify is <strong>{reported ? "on" : "off"}</strong>.
        </p>
      )}

      <div className="pj-changed" role="status">
        {setSwitch.data !== undefined && <Report answer={setSwitch.data} />}
      </div>

      {reported === undefined ? (
        <p className="pj-note">
          The núcleo does not report this switch, so it cannot be changed from
          here.
        </p>
      ) : stale ? (
        <p className="pj-note">
          The switch can be changed again once the rules read is current.
        </p>
      ) : (
        <div className="pj-actions">
          {reported ? (
            <>
              <Button
                variant="ghost"
                disabled={busy}
                onClick={() => send(false)}
              >
                Switch off
              </Button>
              <Button
                variant="ghost"
                disabled={busy}
                onClick={() => send(true)}
              >
                Provision again
              </Button>
            </>
          ) : (
            <ConfirmButton
              variant="ghost"
              label="Switch on"
              confirmLabel="Switch on — write into every IDE worktree"
              disabled={busy}
              onConfirm={() => send(true)}
            />
          )}
        </div>
      )}

      {setSwitch.isError && <IdeVerifyError error={setSwitch.error} />}
    </Panel>
  );
}

const STATE_WORDS: Record<IdeVerifyState, string> = {
  provisioned: "provisioned",
  removed: "removed",
  not_provisioned: "not provisioned",
  untouched: "untouched",
};

function Report({ answer }: { answer: IdeVerifyAnswer }) {
  const counts = new Map<IdeVerifyState, number>();
  for (const worktree of answer.worktrees) {
    counts.set(worktree.state, (counts.get(worktree.state) ?? 0) + 1);
  }
  const summary =
    answer.worktrees.length === 0
      ? "No worktrees."
      : [...counts]
          .map(([state, n]) => `${n} ${STATE_WORDS[state] ?? state}`)
          .join(", ") + ".";
  return (
    <>
      <p className="pj-changed-said">{summary}</p>
      {answer.worktrees.map((worktree) => (
        <p key={worktree.path} className="pj-note">
          <code className="pj-verbatim">{worktree.path}</code> —{" "}
          {STATE_WORDS[worktree.state] ?? worktree.state}
          {worktree.reason !== undefined && worktree.reason !== "" && (
            <> ({worktree.reason})</>
          )}
        </p>
      ))}
    </>
  );
}

function IdeVerifyError({ error }: { error: unknown }) {
  if (!isApiRefusal(error)) {
    return (
      <ErrorNote>the núcleo did not answer — IDE verify is unchanged</ErrorNote>
    );
  }
  return (
    <RefusalNote
      refusal={error}
      sentences={{
        forbidden: "only the owner's key can switch IDE verify",
        not_found: "the núcleo has no row for this project",
      }}
    />
  );
}
