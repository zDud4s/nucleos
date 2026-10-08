import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { apiFetch } from "./client";
import { keys } from "./keys";

/**
 * How long a capture request waits for the owner — `GET`/`POST /config/capture-wait`.
 *
 * Whole minutes; `0` turns capture requests off and the daemon refuses anything above a week
 * (10080) or below zero with a 422 the page shows as a refusal.
 */
export interface CaptureWaitReading {
  minutes: number;
}

export function useCaptureWait() {
  return useQuery({
    queryKey: keys.system.captureWait,
    queryFn: () => apiFetch<CaptureWaitReading>("/config/capture-wait"),
  });
}

/**
 * Stores the wait — `POST /config/capture-wait`. `retry: false`: a refusal is settled, and the
 * answer to "what is stored now" is the daemon's, so the cache is refetched, never written.
 */
export function useSetCaptureWait() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (minutes: number) =>
      apiFetch<CaptureWaitReading>("/config/capture-wait", {
        method: "POST",
        body: JSON.stringify({ minutes }),
      }),
    retry: false,
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: keys.system.captureWait });
    },
  });
}
