import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { apiFetch } from "./client";
import { keys } from "./keys";

/**
 * How capture questions are asked — `GET`/`POST /config/capture`.
 *
 * `answer_days` is how long a question waits for the owner (`0` turns capture questions off, at most
 * 30); `digest_hour` is the local hour (0-23) the daily digest is sent. The daemon refuses a value
 * out of range with a 422 the page shows as a refusal.
 */
export interface CaptureConfigReading {
  answer_days: number;
  digest_hour: number;
}

export function useCaptureConfig() {
  return useQuery({
    queryKey: keys.system.captureConfig,
    queryFn: () => apiFetch<CaptureConfigReading>("/config/capture"),
  });
}

/**
 * Stores one or both fields — `POST /config/capture`. `retry: false`: a refusal is settled, and the
 * answer to "what is stored now" is the daemon's, so the cache is refetched, never written.
 */
export function useSetCaptureConfig() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (change: Partial<CaptureConfigReading>) =>
      apiFetch<CaptureConfigReading>("/config/capture", {
        method: "POST",
        body: JSON.stringify(change),
      }),
    retry: false,
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: keys.system.captureConfig });
    },
  });
}
