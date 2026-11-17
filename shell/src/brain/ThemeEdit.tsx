import { useState } from "react";
import { DAEMON_NAME } from "../copy";
import { ApiRefusal } from "../data/client";
import { useMergeTheme, useRenameTheme } from "../data/themes";
import { Button, ErrorNote, Select } from "../ui";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "../ui/vendor/dropdown-menu";
import "./ThemesProposed.css";

/** A refused rename in the owner's words; anything else means the daemon did not answer. */
function renameFailure(error: unknown): string {
  if (error instanceof ApiRefusal) return "the name was not changed: it must be 1 to 80 characters";
  return `the ${DAEMON_NAME} is not responding: the name was not changed`;
}

/** A refused merge in the owner's words; anything else means the daemon did not answer. */
function mergeFailure(error: unknown): string {
  if (error instanceof ApiRefusal) return "the merge did not go through: the theme may already have been decided";
  return `the ${DAEMON_NAME} is not responding: the merge did not go through`;
}

export interface ThemeChoice {
  id: number;
  name: string;
}

/**
 * Rename and merge controls for one theme, shared by the proposed and accepted lists. `others` are
 * the other live themes the list shows; the theme itself is filtered out here, since the daemon
 * refuses a merge into itself.
 */
export function ThemeEdit({ id, name, others }: { id: number; name: string; others: ThemeChoice[] }) {
  const rename = useRenameTheme();
  const merge = useMergeTheme();
  const [mode, setMode] = useState<"idle" | "rename" | "merge">("idle");
  const [draft, setDraft] = useState(name);
  const targets = others.filter((t) => t.id !== id);
  const [into, setInto] = useState<number | null>(null);
  const busy = rename.isPending || merge.isPending;

  const close = () => setMode("idle");
  const trimmed = draft.trim();
  const canSave = trimmed !== "" && trimmed !== name && !busy;
  const target = into ?? targets[0]?.id ?? null;

  const save = () => {
    if (!canSave) return;
    rename.mutate({ id, name: trimmed }, { onSuccess: close });
  };
  const confirm = () => {
    if (target === null || busy) return;
    merge.mutate({ id, into: target }, { onSuccess: close });
  };

  return (
    <>
      {mode === "idle" && (
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <Button variant="quiet" aria-label={`More actions for theme ${id}`}>
              ⋯
            </Button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end">
            <DropdownMenuItem
              aria-label={`Rename theme ${id}`}
              onSelect={() => {
                rename.reset();
                setDraft(name);
                setMode("rename");
              }}
            >
              Rename
            </DropdownMenuItem>
            <DropdownMenuItem
              aria-label={`Merge theme ${id}`}
              disabled={targets.length === 0}
              onSelect={() => {
                merge.reset();
                setInto(null);
                setMode("merge");
              }}
            >
              Merge
            </DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
      )}
      {mode === "rename" && (
        <>
          <input
            type="text"
            aria-label={`New name for theme ${id}`}
            value={draft}
            maxLength={80}
            autoFocus
            onChange={(e) => setDraft(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") {
                e.preventDefault();
                save();
              } else if (e.key === "Escape") {
                close();
              }
            }}
          />
          <Button variant="quiet" aria-label={`Save name of theme ${id}`} disabled={!canSave} onClick={save}>
            Save
          </Button>
          <Button variant="quiet" aria-label={`Cancel renaming theme ${id}`} onClick={close}>
            Cancel
          </Button>
        </>
      )}
      {mode === "merge" && (
        <>
          <Select
            aria-label={`Merge theme ${id} into`}
            value={target ?? ""}
            onChange={(e) => setInto(Number(e.target.value))}
          >
            {targets.map((t) => (
              <option key={t.id} value={t.id}>
                {t.name} #{t.id}
              </option>
            ))}
          </Select>
          <Button
            variant="quiet"
            aria-label={`Confirm merging theme ${id}`}
            disabled={target === null || busy}
            onClick={confirm}
          >
            Confirm
          </Button>
          <Button variant="quiet" aria-label={`Cancel merging theme ${id}`} onClick={close}>
            Cancel
          </Button>
        </>
      )}
      {rename.isError && <ErrorNote>{renameFailure(rename.error)}</ErrorNote>}
      {merge.isError && <ErrorNote>{mergeFailure(merge.error)}</ErrorNote>}
    </>
  );
}
