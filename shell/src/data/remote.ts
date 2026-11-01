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
  /** An owner device: Admin level plus what only the host reached until now. */
  owner: boolean;
}

/** `POST /remote/pairing-codes` body. `owner` needs `access_level: "admin"`. */
export interface NewPairingCode {
  device_name: string;
  access_level: ApiTokenLevel;
  owner?: boolean;
}

/** `201` answer. `code` exists only here: the daemon never says it again. */
export interface PairingCode {
  code: string;
  access_level: ApiTokenLevel;
  device_name: string;
  expires_at: string;
  owner: boolean;
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

/** What `GET /remote/setup` says about Tailscale on the host. */
export interface TailscaleState {
  installed: boolean;
  signed_in: boolean;
  dns_name: string | null;
  https_ok: boolean;
}

/** `GET /remote/setup`: control token only, so the host's own shell reads it and a device never does. */
export interface RemoteSetup {
  enabled: boolean;
  public_origin: string;
  tailscale: TailscaleState;
  serving: boolean;
  /** Port 443 is open to the public internet (Funnel): setup will not turn on while it is. */
  funnel: boolean;
  /** Port 443 already serves something that is not the gateway: setup leaves it alone. */
  port_taken: boolean;
}

const SETUP_KEY = ["remote", "setup"] as const;

export function useRemoteSetup() {
  return useQuery({
    queryKey: SETUP_KEY,
    queryFn: () => apiFetch<RemoteSetup>("/remote/setup"),
  });
}

/** `POST /remote/setup`: no origin means "this machine's Tailscale name". */
export function useEnableRemote() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (publicOrigin: string | null) =>
      apiFetch<RemoteSetup>("/remote/setup", {
        method: "POST",
        body: JSON.stringify(publicOrigin === null ? {} : { public_origin: publicOrigin }),
      }),
    retry: false,
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: SETUP_KEY });
    },
  });
}

/** `DELETE /remote/setup`. */
export function useDisableRemote() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: () => apiFetch<RemoteSetup>("/remote/setup", { method: "DELETE" }),
    retry: false,
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: SETUP_KEY });
    },
  });
}
