import { emitSessionLost, type Transport } from "../data/transport";
import { stepUp } from "./StepUp";

/**
 * The browser's way in: same-origin `/api` on the gateway, authenticated by the
 * HttpOnly session cookie the browser attaches itself. There is no token to
 * read and no Authorization header; the CSRF header is what the gateway
 * requires on every `/api/*` and `/auth/*` request, GET included.
 */
export const remoteTransport: Transport = {
  baseUrl: "/api",
  credentials: "same-origin",
  async headers() {
    return { "X-Nucleos-Csrf": "1" };
  },
  // A 401 means the session is gone (revoked, expired or never there). There is
  // no credential to re-read, so tell the app and make the 401 final.
  async onUnauthorized() {
    emitSessionLost();
    return "fail";
  },
  // A grave action refused with `step_up_required`: confirm with the passkey, then the client repeats
  // the request once. Only this transport has the hook; the desktop shell is never asked.
  onStepUp: stepUp,
  label: "the remote gateway",
  unauthorizedCode: "session_invalid",
  probeHeaders: { "X-Nucleos-Csrf": "1" },
};
