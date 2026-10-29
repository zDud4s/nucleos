import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { apiFetch } from "./client";
import type { ApiTokenLevel } from "./system";

/** Owner side of remote access: pairing codes and paired devices (`core/src/http/remote.rs`). */

/** Query key of the device list. The pairing code is never under any key. */
const DEVICES_KEY = ["remote", "devices"] as const;

/** One paired device — `GET /remote/devices`. Carries no secret. */
export interface RemoteDevice {
  id: number;
  name: string;
  access_level: ApiTokenLevel;
  created_at: string;
  last_seen_at: string | null;
  revoked_at: string | null;
}

/** `POST /remote/pairing-codes` body. */
export interface NewPairingCode {
  device_name: string;
  access_level: ApiTokenLevel;
}

/** `201` answer. `code` exists only here: the daemon never says it again. */
export interface PairingCode {
  code: string;
  access_level: ApiTokenLevel;
  device_name: string;
  expires_at: string;
}

/** The paired devices. No poll: the list changes only through this panel or a pairing. */
export function useRemoteDevices() {
  return useQuery({
    queryKey: DEVICES_KEY,
    queryFn: () => apiFetch<RemoteDevice[]>("/remote/devices"),
  });
}

/**
 * Mint a pairing code. The caller keeps the answer in component state; it is
 * never written into the query cache, because it cannot be refetched.
 */
export function useCreatePairingCode() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (body: NewPairingCode) =>
      apiFetch<PairingCode>("/remote/pairing-codes", {
        method: "POST",
        body: JSON.stringify(body),
      }),
    retry: false,
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: DEVICES_KEY });
    },
  });
}

/** `DELETE /remote/devices/{id}` answers 204 (idempotent), 404 for an unknown id. */
export function useRevokeDevice() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (id: number) => apiFetch<void>(`/remote/devices/${id}`, { method: "DELETE" }),
    retry: false,
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: DEVICES_KEY });
    },
  });
}
