import { useQueries, useQueryClient } from "@tanstack/react-query";
import { useEffect, useMemo, useRef, useState } from "react";
import {
  abortChatTransfer,
  beginChatTransfer,
  finishChatTransfer,
  importChatTransfer,
  packageChatTransfer,
  previewChatTransfer,
  useChatProject,
  type ChatTransferPreview,
} from "../data/chats";
import { apiFetch, isApiRefusal } from "../data/client";
import { chatMachineOf, machinesForProject, rememberChatMachine } from "../data/chatMachines";
import { keys } from "../data/keys";
import { pairedMachine, type MachineId } from "../data/machineStore";
import { useMachines, withMachine } from "../data/machines";
import { requestNativeStepUp } from "../remote/nativeTransport";
import { isRemote } from "../remote/host";
import { Button, ErrorNote, Modal } from "../ui";

type Project = { project_root: string | null; remote?: string | null };
type When = "after_turn" | "now";

export function unreachableOriginNotice(source: MachineId, reachable: boolean): string | null {
  return source === "here" || reachable ? null : "This chat's machine is unreachable, so it cannot be moved.";
}

function message(error: unknown): string {
  if (isApiRefusal(error)) return error.detail;
  return error instanceof Error && error.message !== "" ? error.message : "The move could not be completed.";
}

async function stepUp(machine: MachineId): Promise<void> {
  if (machine === "here") return;
  const paired = pairedMachine(machine);
  if (paired === undefined) throw new Error("The destination machine is no longer paired.");
  const outcome = await requestNativeStepUp(paired, "Move chat");
  if (outcome.kind === "failed") throw new Error(outcome.detail);
}

/** Runs the frozen half of a move, restoring the origin whenever it cannot finish. */
export async function finalizeChatMove<T>({
  begin,
  packageChat,
  importChat,
  finish,
  abort,
}: {
  begin: () => Promise<void>;
  packageChat: () => Promise<unknown>;
  importChat: (packageBody: unknown) => Promise<T>;
  finish: () => Promise<void>;
  abort: () => Promise<void>;
}): Promise<T> {
  let started = false;
  try {
    await begin();
    started = true;
    const packageBody = await packageChat();
    const imported = await importChat(packageBody);
    await finish();
    return imported;
  } catch (error) {
    if (started) await abort();
    throw error;
  }
}

/** The confirmation dialog and transaction for moving one chat to another machine. */
export function MoveChatDialog({
  chatId,
  open,
  onOpenChange,
}: {
  chatId: string;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  const source = chatMachineOf(chatId);
  const project = useChatProject(chatId);
  const machines = useMachines();
  const queryClient = useQueryClient();
  const cwd = project.data?.cwd ?? null;
  const projectLists = useQueries({
    queries: machines.map((machine) => ({
      queryKey: withMachine(keys.projects.all, machine.id),
      queryFn: () => apiFetch<Project[]>("/projects", undefined, machine.id),
      enabled: cwd !== null && machine.reachable,
      staleTime: 60_000,
    })),
  });
  const sourceIndex = machines.findIndex((machine) => machine.id === source);
  const sourceProject = useMemo(() => {
    if (cwd === null) return null;
    const found = sourceIndex < 0 ? undefined : projectLists[sourceIndex].data?.find((item) => item.project_root === cwd);
    return found === undefined ? undefined : { machine: source, remote: found.remote ?? null };
  }, [cwd, projectLists, source, sourceIndex]);
  const offered =
    cwd !== null && sourceProject === undefined
      ? [source]
      : machinesForProject(
          sourceProject,
          machines.map((machine) => machine.id),
          machines.map((machine, index) => ({ machine: machine.id, projects: projectLists[index].data })),
        );
  const destinations = isRemote() ? [] : machines.filter((machine) => machine.id !== source && offered.includes(machine.id));
  const sourceView = machines.find((machine) => machine.id === source);
  const originProblem = unreachableOriginNotice(source, sourceView?.reachable === true);
  const originReachable = originProblem === null;
  const [destination, setDestination] = useState<MachineId | null>(null);
  const [when, setWhen] = useState<When>("after_turn");
  const [preview, setPreview] = useState<ChatTransferPreview | null>(null);
  const [progress, setProgress] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const begun = useRef<{ transferId: string; done: boolean } | null>(null);

  useEffect(() => {
    if (destination === null || !destinations.some((machine) => machine.id === destination)) {
      setDestination(destinations[0]?.id ?? null);
    }
  }, [destination, destinations]);

  const cancel = async () => {
    const active = begun.current;
    if (active !== null && !active.done) {
      setProgress("Restoring the chat on its original machine…");
      try {
        await abortChatTransfer(chatId, active.transferId, source);
      } catch {
        setError("The move was cancelled, but the original machine could not confirm that the chat was restored.");
        return;
      }
    }
    begun.current = null;
    onOpenChange(false);
  };

  const inspect = async () => {
    if (!originReachable) {
      setError(originProblem ?? "This chat's machine is unreachable, so it cannot be moved.");
      return;
    }
    if (destination === null) return;
    setError(null);
    setProgress("Checking what will move…");
    try {
      const next = await previewChatTransfer(chatId, source);
      setPreview(next);
      if (!next.target_ok) setError(next.target_problem ?? "This chat cannot be moved from its current project.");
    } catch (cause) {
      setError(message(cause));
    } finally {
      setProgress(null);
    }
  };

  const move = async () => {
    if (destination === null || preview === null || !preview.target_ok) return;
    const transferId = crypto.randomUUID();
    setError(null);
    try {
      setProgress("Waiting for passkey confirmation…");
      await stepUp(source);
      await stepUp(destination);
      await finalizeChatMove({
        begin: async () => {
          setProgress(when === "after_turn" ? "Waiting for the current turn to finish…" : "Freezing the chat…");
          await beginChatTransfer(chatId, { when, transfer_id: transferId }, source);
          begun.current = { transferId, done: false };
        },
        packageChat: async () => {
          setProgress("Packaging the chat…");
          return await packageChatTransfer(chatId, transferId, source);
        },
        importChat: async (packageBody) => {
          setProgress("Importing the chat on the destination…");
          return await importChatTransfer(transferId, packageBody as Parameters<typeof importChatTransfer>[1], destination);
        },
        finish: async () => {
          setProgress("Completing the move…");
          await finishChatTransfer(chatId, { transfer_id: transferId, to: destination }, source);
        },
        abort: async () => await abortChatTransfer(chatId, transferId, source),
      });
      if (begun.current !== null) begun.current.done = true;
      rememberChatMachine([chatId], destination);
      await queryClient.invalidateQueries({ queryKey: keys.chats.all });
      onOpenChange(false);
    } catch (cause) {
      begun.current = null;
      setError(message(cause));
    } finally {
      setProgress(null);
    }
  };

  const busy = progress !== null;
  return (
    <Modal
      open={open}
      onOpenChange={(next) => {
        if (next) onOpenChange(true);
        else void cancel();
      }}
      title="Move chat"
      footer={
        <>
          <Button variant="ghost" disabled={busy} onClick={() => void cancel()}>
            Cancel
          </Button>
          {preview === null ? (
            <Button variant="approve" disabled={busy || destination === null || !originReachable} onClick={() => void inspect()}>
              Review move
            </Button>
          ) : (
            <Button variant="approve" disabled={busy || !preview.target_ok} onClick={() => void move()}>
              Move chat
            </Button>
          )}
        </>
      }
    >
      <div className="chats-helpers">
        <p>The current turn can finish and the queue will move with this chat.</p>
        {originProblem !== null && <ErrorNote>{originProblem}</ErrorNote>}
        <label>
          Move to
          <select aria-label="Move chat to" value={destination ?? ""} onChange={(event) => { setDestination(event.target.value); setPreview(null); setError(null); }} disabled={busy}>
            {destinations.map((machine) => <option key={machine.id} value={machine.id} disabled={!machine.reachable}>{machine.name}</option>)}
          </select>
        </label>
        {destinations.length === 0 && cwd !== null && <p>No other reachable machine has this chat's project.</p>}
        {preview?.running === true && (
          <fieldset disabled={busy}>
            <legend>Running turn</legend>
            <label><input type="radio" checked={when === "after_turn"} onChange={() => setWhen("after_turn")} /> Let it finish; new messages wait and move in the queue.</label>
            <label><input type="radio" checked={when === "now"} onChange={() => setWhen("now")} /> Move now and cut the running turn.</label>
          </fieldset>
        )}
        {preview !== null && <TransferNotes preview={preview} />}
        {progress !== null && <p role="status">{progress}</p>}
        {error !== null && <ErrorNote>{error}</ErrorNote>}
      </div>
    </Modal>
  );
}

export function TransferNotes({ preview }: { preview: ChatTransferPreview }) {
  return <>
    <h2>Before you move</h2>
    <p>A running turn can finish first; messages added while it finishes move in the queue.</p>
    <h3>What stops</h3>
    {preview.will_stop.length === 0 ? <p>Nothing else will stop.</p> : <ul>{preview.will_stop.map((item) => <li key={item}>{item}</li>)}</ul>}
    <h3>What stays behind</h3>
    {preview.left_behind.length === 0 ? <p>No ignored files stay behind.</p> : <ul>{preview.left_behind.map((item) => <li key={item}>{item}</li>)}</ul>}
  </>;
}
