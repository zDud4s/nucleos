import { useSandboxStatus } from "../data/sandbox";

/**
 * The global line about the OS sandbox, shown only when it is "refused": runs are blocked, and that
 * is the one state every page must say. Every other state (no backend, off, armed) and the unknown
 * answer stay quiet here; the Health view carries the indicator.
 */
export function SandboxBanner() {
  const status = useSandboxStatus();
  if (status.data?.state !== "refused") return null;
  return (
    <div role="status" aria-label="Sandbox" className="sandbox-banner">
      {status.data.banner ?? "Sandbox refused: runs are blocked"}
    </div>
  );
}
