import { useState } from "react";
import { isApiRefusal } from "../data/client";
import { useProjectOwnership, useWriteProjectFile, type Claim } from "../data/project-config";
import { useProjectCat } from "../data/projects";
import { Quiet } from "../ui";

/**
 * The files this app is the legitimate author of, and the editor for them.
 *
 * The other half of layer 1. `Settings` next door is the app's config that lives in the database
 * and gets a form; this is the app's config that lives on disk and gets a **raw editor with
 * validation on save**.
 *
 * **Why a raw editor and not a form, when a form is what the design asked for.** A form would have
 * to read the YAML, set a field, and write the whole file back — and re-serialising YAML destroys
 * comments. `.ai/autopilot.yaml`'s own loader has a branch for a file that is nothing but comments,
 * with a note saying why: commenting the `gate_command:` line out is how somebody switches a gate
 * off for an afternoon. A form here would silently delete the note explaining why a schedule is
 * disabled, which is precisely the "second author of a document git already owns" failure the old
 * rule was written against. The rule stops applying to *whether* the app writes this file; it does
 * not stop applying to *how*.
 *
 * What makes the raw editor better than an editor rather than merely equal to one is the save: the
 * daemon holds the text to the same parser the daemon reads it with, so a typo'd key cannot be
 * saved at all — `AutopilotRules` is `deny_unknown_fields`, and `gate_commmand:` in `vim` saves
 * happily and leaves the project ungated for ever.
 *
 * **Nothing here hard-codes a path.** The list comes from `GET /projects/{id}/ownership`, so the
 * fence the page draws and the fence the daemon enforces are the same fence.
 */

export interface OwnedFilesProps {
  projectId: string;
}

export function OwnedFiles({ projectId }: OwnedFilesProps) {
  const ownership = useProjectOwnership(projectId);

  if (ownership.data === undefined) {
    return <p className="text-sm text-text-faint">Reading the write boundary…</p>;
  }

  /*
    Nothing claimed is a whole section saying so, and it used to say so twice: once that the app
    authors nothing here, and once that everything else belongs to the repository — which, with
    nothing claimed, is the same sentence. One line, and the boundary itself behind the question.
  */
  if (ownership.data.length === 0) {
    return (
      <Quiet says="none">
        Every file in this project belongs to the repository, so all of it is read here and edited
        where code is edited. Which files the app may write is answered by the núcleo rather than
        decided by this page, so an empty list is a fence rather than a gap.
      </Quiet>
    );
  }

  return (
    <div className="flex flex-col gap-3">
      {ownership.data.map((claim) => (
        <OwnedFile key={claim.path} projectId={projectId} claim={claim} />
      ))}

      {/*
        The other layer, said out loud rather than left to be discovered by a missing button. A
        reader who does not know the boundary exists reads "no edit button" as an oversight.
      */}
      <p className="text-xs text-text-faint">
        Every other file in this project belongs to the repository. Those are read here and edited in
        VS Code, at the file and the line — the Code mode has the door.
      </p>
    </div>
  );
}

/**
 * What each refusal means, in the page's words.
 *
 * `invalid` is deliberately absent: that one arrives with the parser's own sentence, which locates
 * the broken line, and no copy written here could beat it.
 */
const REFUSALS: Record<string, string> = {
  kill_switch:
    "the emergency stop is engaged, so nothing writes — including this. The file is still editable in an editor.",
  no_project_root: "the núcleo has no folder recorded for this project.",
  not_ours: "the núcleo does not consider this app the author of that file.",
  unwritable: "that path does not land inside the project folder.",
  internal: "the núcleo hit an error of its own while writing.",
};

function OwnedFile({ projectId, claim }: { projectId: string; claim: Claim }) {
  const [open, setOpen] = useState(false);
  const [draft, setDraft] = useState<string | null>(null);
  const file = useProjectCat(projectId, claim.path, open);
  const write = useWriteProjectFile();

  /**
   * A file that is not there yet is a state, not an error.
   *
   * `cat` answers 404 for it, and a project with no rules file is the ordinary case for a project
   * nobody has scheduled anything in. Saving creates it — which is the one thing this editor can do
   * that reading cannot.
   */
  const missing = file.isError && isApiRefusal(file.error) && file.error.status === 404;
  const loaded = file.data ?? (missing ? "" : null);
  const text = draft ?? loaded;
  const dirty = draft !== null && draft !== loaded;

  const refused = write.isError && isApiRefusal(write.error) ? write.error : null;

  return (
    <div className="rounded-lg border border-border bg-surface p-4">
      <div className="flex flex-wrap items-baseline justify-between gap-2">
        <p className="font-mono text-sm text-text">{claim.path}</p>
        <button
          type="button"
          onClick={() => setOpen(!open)}
          className="rounded-md border border-border px-2 py-1 text-xs text-text-muted hover:border-border-strong"
        >
          {open ? "close" : "edit"}
        </button>
      </div>
      <p className="mt-1 text-xs text-text-muted">{claim.what}</p>

      {open ? (
        text === null ? (
          <p className="mt-3 text-xs text-text-faint">Reading the file…</p>
        ) : (
          <div className="mt-3 flex flex-col gap-2">
            {missing ? (
              <p className="text-xs text-text-faint">
                There is no such file yet. Saving will create it.
              </p>
            ) : null}
            <textarea
              aria-label={claim.path}
              value={text}
              spellCheck={false}
              rows={14}
              onChange={(event) => setDraft(event.target.value)}
              className="w-full rounded-md border border-border bg-surface-sunken p-2 font-mono text-xs text-text"
            />
            <div className="flex flex-wrap items-center gap-2">
              <button
                type="button"
                disabled={!dirty || write.isPending}
                onClick={() =>
                  write.mutate(
                    { projectId, path: claim.path, contents: text },
                    // Cleared only on success, so a refused save leaves the text the person wrote
                    // in front of them. Dropping it would make a rejected edit an edit lost.
                    { onSuccess: () => setDraft(null) },
                  )
                }
                className="rounded-md border border-border px-3 py-1.5 text-xs text-text enabled:hover:border-border-strong disabled:opacity-40"
              >
                {write.isPending ? "saving…" : "save"}
              </button>
              {dirty ? <span className="text-xs text-text-faint">unsaved changes</span> : null}
              {write.isSuccess && !dirty ? (
                <span className="text-xs text-tone-active-fg">saved</span>
              ) : null}
            </div>

            {refused !== null ? (
              <p className="rounded-md border border-tone-danger-border bg-tone-danger-bg p-2 text-xs text-text-muted">
                {/*
                  The parser's own words for `invalid`, because "unprocessable entity" sends
                  somebody to a text editor to find out where the file broke — which is the surface
                  this editor exists to replace. Nothing was written: the daemon validates first, so
                  the file on disk is still the one that was there.
                */}
                {REFUSALS[refused.code] ?? refused.detail}
              </p>
            ) : null}
          </div>
        )
      ) : null}
    </div>
  );
}
