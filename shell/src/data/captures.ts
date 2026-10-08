import { keepPreviousData, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { apiFetch } from "./client";
import { keys } from "./keys";
import { POLL, backgroundCadence } from "./poll";

/**
 * Capture requests, as hooks — the daemon's questions to the owner over `/capture-requests`.
 *
 * | what                    | route                                      |
 * |-------------------------|--------------------------------------------|
 * | open ones               | `GET /capture-requests`                    |
 * | every state             | `GET /capture-requests?state=all`          |
 * | dismiss one             | `POST /capture-requests/{job_id}/dismiss`  |
 * | answer one (as a note)  | `POST /capture-requests/{job_id}/answer`   |
 */

export type CaptureState = "open" | "answered" | "dismissed" | "expired";

export interface CaptureRequest {
  job_id: number;
  project_id: string;
  causes: string[];
  prompt_text: string;
  state: CaptureState;
  deadline: string;
  seconds_left: number;
  note_id: number | null;
  created_at: string;
  closed_at: string | null;
}

/**
 * The open requests.
 *
 * Polled in the background at the fast cadence: this is also the Brain badge, on screen on every
 * page, and a badge that stops counting when the window loses focus lies about what is waiting.
 */
export function useOpenCaptures() {
  return useQuery({
    queryKey: keys.captures.open,
    queryFn: () => apiFetch<CaptureRequest[]>("/capture-requests"),
    refetchInterval: backgroundCadence(POLL.fast),
    refetchIntervalInBackground: true,
    placeholderData: keepPreviousData,
  });
}

/** Every request in every state, for the history. */
export function useAllCaptures() {
  return useQuery({
    queryKey: keys.captures.everything,
    queryFn: () => apiFetch<CaptureRequest[]>("/capture-requests?state=all"),
    refetchInterval: backgroundCadence(POLL.queue),
    placeholderData: keepPreviousData,
  });
}

function useCaptureMutation<V, R>(mutationFn: (input: V) => Promise<R>) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn,
    // A write is settled; a retried answer would be a second note.
    retry: false,
    // `onSettled`: a failed write (409 closed) still leaves the screen's copy suspect. An answer
    // also creates a note, so the notes are refetched with the requests.
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: keys.captures.all });
      void queryClient.invalidateQueries({ queryKey: keys.ownerNotes.all });
    },
  });
}

export function useAnswerCapture() {
  return useCaptureMutation(({ jobId, text }: { jobId: number; text: string }) =>
    apiFetch<{ note_id: number; released: boolean }>(`/capture-requests/${jobId}/answer`, {
      method: "POST",
      body: JSON.stringify({ text, origin: "shell" }),
    }),
  );
}

export function useDismissCapture() {
  return useCaptureMutation(({ jobId }: { jobId: number }) =>
    apiFetch<unknown>(`/capture-requests/${jobId}/dismiss`, { method: "POST" }),
  );
}
