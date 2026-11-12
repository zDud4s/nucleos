import { useQuery } from "@tanstack/react-query";
import { apiFetch } from "./client";

/**
 * The OS sandbox's state, read-only — `core/src/http/sandbox.rs` over `GET /sandbox/status`.
 * There is no way to change it from here: the owner flips the switch in the daemon's config file.
 */
export interface SandboxStatus {
  state: "no_backend" | "off" | "armed" | "refused";
  /** The text to show; `null` when the sandbox is armed. */
  banner: string | null;
}

export function useSandboxStatus() {
  return useQuery({
    queryKey: ["sandbox", "status"],
    queryFn: () => apiFetch<SandboxStatus>("/sandbox/status"),
    staleTime: 60_000,
  });
}
