import { useSandboxStatus } from "../data/sandbox";

/**
 * The permanent line about the OS sandbox. Honest about what is true: with no backend it says the
 * sandbox is not built yet, never that it is armed. Absent when armed, and while the answer is
 * unknown — a guess either way would be a claim about containment.
 */
export function SandboxBanner() {
  const status = useSandboxStatus();
  const banner = status.data?.banner;
  if (status.data === undefined || status.data.state === "armed" || !banner) return null;
  return (
    <div role="status" aria-label="Sandbox" className="sandbox-banner">
      {banner}
    </div>
  );
}
