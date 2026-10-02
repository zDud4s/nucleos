import type { ReactNode } from "react";
import { useQuery } from "@tanstack/react-query";
import { DAEMON_URL, apiText, isApiRefusal, isApiUnavailable } from "../data/client";
import { keys } from "../data/keys";
import { POLL, backgroundCadence } from "../data/poll";
import { useHealth } from "../data/system";

/**
 * The handshake, as a reading.
 *
 * `connecting` is deliberately not one of the two takeovers: it is what the
 * shell knows before the first probe answers, and it is neither an outage nor a
 * refusal.
 */
type Handshake = "connecting" | "unreachable" | "unauthorised" | "through";

/**
 * Did the núcleo answer, and refuse us?
 *
 * `ApiUnavailable{kind:"token"}` belongs here rather than with the outages. A
 * locked or empty keychain is not the daemon being down — the daemon may be
 * perfectly healthy — and telling someone to wait for a process that is already
 * running is advice that never comes true.
 */
function notAuthorised(error: unknown): boolean {
  if (isApiUnavailable(error)) return error.kind === "token";
  if (isApiRefusal(error)) return error.status === 401 || error.status === 403;
  return false;
}

function unreachable(error: unknown): boolean {
  return isApiUnavailable(error) && error.kind === "transport";
}

export interface ConnectionGateProps {
  children: ReactNode;
}

/**
 * Nothing renders until the núcleo has answered, and *how* it failed to answer
 * decides which page you get.
 *
 * Two full-page takeovers, never one. They look similar and they mean opposite
 * things:
 *
 * - **unreachable** — there was no answer. Waiting is the entire treatment; the
 *   shell keeps asking and lets itself back in the moment the daemon is up.
 * - **not authorised** — there *was* an answer, and it was no. The daemon token
 *   rotates when the daemon restarts, so a window holding the old one will be
 *   refused for as long as it stays open. Waiting fixes nothing, and a screen
 *   that says "retrying…" would be a lie that costs somebody an afternoon.
 *
 * Collapsing the two into one "cannot connect" screen is the specific failure
 * this component exists to prevent. Anything else the daemon does — a 500 on
 * `/status`, a route that is not there — renders the shell: the app is full of
 * pages that do not need `/status`, and a takeover for a single bad route would
 * take the whole window away over nothing.
 */
export function ConnectionGate({ children }: ConnectionGateProps) {
  const health = useHealth();

  /**
   * The first *authenticated* call, and therefore the proof that the credential
   * still works. `/health` is unauthenticated by design, so a shell that
   * stopped at it would happily show a full UI to a window whose token had
   * rotated out from under it.
   *
   * Kept polling in the background for the same reason the health probe is: the
   * token rotates on daemon restart, and a window that comes back from the tray
   * needs to find out immediately rather than on the next thing the user clicks.
   */
  const status = useQuery({
    queryKey: keys.status,
    queryFn: () => apiText("/status"),
    refetchInterval: backgroundCadence(POLL.fast),
    refetchIntervalInBackground: true,
    enabled: health.data === true,
  });

  const reading = read(health.data, status.error);

  if (reading === "connecting") {
    return (
      <div className="app-takeover app-takeover-quiet">
        <div className="app-takeover-card">
          <p className="app-takeover-body">reaching the núcleo…</p>
        </div>
      </div>
    );
  }

  if (reading === "unreachable") {
    return (
      <div className="app-takeover" role="alert">
        <div className="app-takeover-card">
          <h1 className="app-takeover-title">daemon unreachable</h1>
          <p className="app-takeover-body">
            The núcleo is not answering on {DAEMON_URL}. Nothing here is lost — the shell keeps asking, and
            this window comes back on its own the moment the daemon does.
          </p>
          <p className="app-takeover-hint">retrying every {POLL.fast / 1000} seconds</p>
        </div>
      </div>
    );
  }

  if (reading === "unauthorised") {
    return (
      <div className="app-takeover" role="alert">
        <div className="app-takeover-card">
          <h1 className="app-takeover-title">reachable — not authorised</h1>
          <p className="app-takeover-body">
            The núcleo answered and refused this window's credential. The daemon token is rotated when the
            daemon restarts, and this window is still holding the one it read at startup.
          </p>
          <p className="app-takeover-hint">
            Waiting will not clear this. Restart NucleOS so the shell reads the current token.
          </p>
        </div>
      </div>
    );
  }

  return <>{children}</>;
}

/**
 * The order of these tests is the whole logic.
 *
 * A dead daemon outranks a stale credential: when nothing is answering, a token
 * error left over from the last round is a fact about a moment that has passed,
 * and reporting it would send someone to restart an app when the real problem
 * is a process that is not running.
 */
function read(healthy: boolean | undefined, statusError: unknown): Handshake {
  if (healthy === false) return "unreachable";
  if (healthy === undefined) return "connecting";
  if (notAuthorised(statusError)) return "unauthorised";
  if (unreachable(statusError)) return "unreachable";
  return "through";
}
