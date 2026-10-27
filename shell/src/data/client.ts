import { DAEMON_URL, TokenReadError, getTransport } from "./transport";

/**
 * The núcleo, on loopback (defined with the desktop transport). The desktop
 * shell has no second base URL; the remote shell goes through the transport
 * seam instead of this constant.
 */
export { DAEMON_URL };

/**
 * The daemon answered, and the answer was no.
 *
 * A refusal is a *value*: it has a name, and a page that knows the name can say
 * something true ("the kill switch is engaged; nothing autonomous starts")
 * instead of something useless ("request failed"). Collapsing these into a
 * generic error is how a UI ends up offering a retry for a decision that will
 * never change its mind.
 */
export class ApiRefusal extends Error {
  /** The HTTP status the daemon refused with. */
  readonly status: number;
  /**
   * A stable name for this refusal.
   *
   * Either the daemon's own (`{"refusal":"kill_switch"}`) or, when the route
   * refused without naming itself, a derivation of the status that is stable
   * enough for a page to switch on. Never a free-text sentence: sentences get
   * reworded, and a `switch` over them rots silently.
   */
  readonly code: string;
  /** Whatever prose the daemon sent, for the cases where it wrote a better sentence than we would. */
  readonly detail: string;
  /** The parsed JSON body, when the refusal carried one: `consent_required` ships the listing in it. */
  readonly body: unknown;

  constructor(status: number, code: string, detail: string, body?: unknown) {
    super(detail || `${status} ${code}`);
    this.name = "ApiRefusal";
    this.status = status;
    this.code = code;
    this.detail = detail;
    this.body = body;
  }
}

/**
 * There was no answer to refuse with.
 *
 * Deliberately a different class from {@link ApiRefusal}, because the two want
 * opposite responses from a page: a refusal is settled and a retry is
 * pointless; an absence clears by waiting and a retry is the whole treatment.
 * A page that cannot tell them apart either nags the user about a decision or
 * gives up on an outage.
 *
 * `kind` keeps the two ways of having no answer apart for the same reason:
 * `transport` means the daemon is not there, `token` means we never got a
 * credential to ask with — a locked keychain, not a dead process. One of those
 * is fixed by starting the daemon and the other is not.
 */
export class ApiUnavailable extends Error {
  readonly kind: "transport" | "token";
  /**
   * Whatever was actually thrown underneath.
   *
   * Its own field rather than the standard `Error.cause` option, because that
   * constructor overload is ES2022 and this project's `lib` is ES2020 — the
   * two-argument `Error` does not typecheck here.
   */
  readonly reason: unknown;

  constructor(kind: "transport" | "token", message: string, reason?: unknown) {
    super(message);
    this.name = "ApiUnavailable";
    this.kind = kind;
    this.reason = reason;
  }
}

export function isApiRefusal(error: unknown): error is ApiRefusal {
  return error instanceof ApiRefusal;
}

export function isApiUnavailable(error: unknown): error is ApiUnavailable {
  return error instanceof ApiUnavailable;
}

// The credential memo (one read per window, rejections never memoised) lives
// in the desktop transport, see ./transport.ts.

/**
 * Names for the statuses the núcleo refuses with, for routes that refuse
 * without naming themselves.
 *
 * A derivation and not an interpretation: `429` becomes `too_many_requests`,
 * not `budget`, because only the route knows whether a 429 was the budget or
 * something else. Pages that need the sharper meaning match on the daemon's
 * own `refusal` name; this is the floor under them, and its only promise is
 * that the same status always yields the same string.
 */
const STATUS_CODES: Record<number, string> = {
  400: "bad_request",
  401: "unauthorized",
  403: "forbidden",
  404: "not_found",
  409: "conflict",
  422: "unprocessable",
  423: "locked",
  429: "too_many_requests",
  500: "internal",
  503: "unavailable",
};

function codeForStatus(status: number): string {
  return STATUS_CODES[status] ?? `http_${status}`;
}

/**
 * Read a refusal out of whatever the route chose to send.
 *
 * The núcleo refuses in three shapes — `{"refusal":"name"}` from the routes
 * that named their refusals, `{"error":"sentence"}` from a few others, and
 * bare `(StatusCode, String)` prose from most of the rest — plus a fourth,
 * empty, from the handlers that return a status and nothing else. All four
 * have to become the same value here, or every page would have to know which
 * shape its route picked.
 *
 * Nothing in here may throw: this runs on the failure path, and a parse error
 * while explaining a failure would replace a refusal we understand with an
 * exception we do not.
 */
async function refusalFrom(res: Response): Promise<ApiRefusal> {
  let text = "";
  try {
    text = await res.text();
  } catch {
    // A body that cannot be read still leaves us the status, which is the part
    // that decides what a page does.
  }

  const fallback = codeForStatus(res.status);
  const trimmed = text.trim();
  if (trimmed === "") return new ApiRefusal(res.status, fallback, res.statusText ?? "");

  let parsed: unknown = undefined;
  try {
    parsed = JSON.parse(trimmed);
  } catch {
    // Prose, then — which is a perfectly good detail, just not a name.
    return new ApiRefusal(res.status, fallback, trimmed);
  }

  if (typeof parsed === "object" && parsed !== null) {
    const body = parsed as Record<string, unknown>;
    if (typeof body.refusal === "string" && body.refusal !== "") {
      // The daemon named it. Its name wins over anything we could derive.
      //
      // A `detail` beside the name wins as the sentence, and that pair is not
      // decoration: `POST /projects/{id}/write` refuses invalid YAML with
      // `{refusal:"invalid", detail:"...line 3, column 5"}`, and an editor that
      // showed only "invalid" would send somebody to a text editor to find out
      // where — which is the surface that editor exists to replace. Without a
      // detail the name is still the sentence, which is what every route that
      // sends only a name has always got.
      const detail = typeof body.detail === "string" && body.detail !== "" ? body.detail : body.refusal;
      return new ApiRefusal(res.status, body.refusal, detail, body);
    }
    const prose = [body.error, body.message, body.reason].find((v) => typeof v === "string" && v !== "");
    if (typeof prose === "string") return new ApiRefusal(res.status, fallback, prose);
  }

  return new ApiRefusal(res.status, fallback, trimmed);
}

/**
 * A 401 under the desktop transport most likely means the daemon restarted and
 * rotated its token, so the transport drops the memoised one and asks for one
 * retry. Safe for mutations too — a 401 means the request was never acted on. A
 * second 401, or a transport that answers "fail" (the remote session is gone),
 * is a real refusal.
 */
async function request(path: string, init: RequestInit | undefined): Promise<Response> {
  const transport = getTransport();
  try {
    return await requestOnce(transport, path, init);
  } catch (error) {
    if (!(error instanceof ApiRefusal) || error.status !== 401) throw error;
    const decision = await transport.onUnauthorized();
    if (decision === "retry") return await requestOnce(transport, path, init);
    if (transport.unauthorizedCode !== undefined && error.code !== transport.unauthorizedCode) {
      throw new ApiRefusal(401, transport.unauthorizedCode, error.detail, error.body);
    }
    throw error;
  }
}

async function requestOnce(
  transport: ReturnType<typeof getTransport>,
  path: string,
  init: RequestInit | undefined,
): Promise<Response> {
  let authHeaders: Record<string, string>;
  try {
    authHeaders = await transport.headers();
  } catch (cause) {
    if (cause instanceof ApiUnavailable) throw cause;
    const reason = cause instanceof TokenReadError ? cause.reason : cause;
    throw new ApiUnavailable("token", "the daemon token could not be read", reason);
  }

  const headers = new Headers(init?.headers);
  for (const [name, value] of Object.entries(authHeaders)) headers.set(name, value);
  // Set only when we are actually sending something, and never over a caller
  // that chose its own type — a multipart upload picks its own boundary and
  // must not be told it is JSON.
  if (init?.body !== undefined && init.body !== null && !headers.has("Content-Type")) {
    headers.set("Content-Type", "application/json");
  }

  let res: Response;
  try {
    res = await fetch(`${transport.baseUrl}${path}`, { credentials: transport.credentials, ...init, headers });
  } catch (cause) {
    throw new ApiUnavailable("transport", "the daemon did not answer", cause);
  }

  if (!res.ok) throw await refusalFrom(res);
  return res;
}

/**
 * An authenticated streaming GET: the body, unread. Refusals arrive as
 * {@link ApiRefusal} through `request`, exactly as for a JSON call.
 */
export async function openStream(path: string, signal: AbortSignal): Promise<ReadableStream<Uint8Array>> {
  const res = await request(path, { signal });
  if (res.body === null) throw new Error(`the daemon answered ${path} with no body`);
  return res.body;
}

/**
 * One authenticated JSON call.
 *
 * There are no automatic retries anywhere below this line, on reads or on
 * writes. Refusals are semantic — a second identical request gets the same
 * answer — and a retried mutation is a second attempt at an action a person
 * asked for once. Retry policy is a decision each hook makes out loud.
 *
 * A `204` is a real answer and not a body: the routes that mutate and return
 * nothing (`POST /autopilot/kill`, `POST /autopilot/attention`) would throw a
 * parse error out of a success. Callers of those type `T` as `void`.
 */
export async function apiFetch<T>(path: string, init?: RequestInit): Promise<T> {
  const res = await request(path, init);
  if (res.status === 204 || res.status === 205) return undefined as T;
  try {
    return (await res.json()) as T;
  } catch {
    // A 200 whose body is not JSON is the daemon breaking its own contract,
    // which is neither a refusal nor an outage — so it gets neither name.
    throw new Error(`the daemon answered ${path} with a body that is not JSON`);
  }
}

/**
 * One authenticated call whose answer is text.
 *
 * `GET /status` is a status *line*, not a document — it predates the JSON
 * routes and stayed text because a person reads it. It exists here so that the
 * one endpoint shaped differently cannot tempt a caller into `res.json()` on
 * prose.
 */
export async function apiText(path: string, init?: RequestInit): Promise<string> {
  const res = await request(path, init);
  return await res.text();
}

/**
 * One authenticated call whose answer is bytes.
 *
 * Three routes answer this way: one attachment (`GET
 * /email/{id}/attachments/{position}`), a file download, and the browser
 * screenshot route. All three are `application/octet-stream`, and neither
 * {@link apiFetch} nor {@link apiText} is safe over them — `res.json()` throws
 * on a body that was never JSON, and `res.text()` decodes the bytes as UTF-8
 * and hands back a string that has silently lost whatever was not valid text.
 * A `Blob` is the one shape a caller can hand to `URL.createObjectURL` or write
 * to disk without either failure mode.
 */
export async function apiBlob(path: string, init?: RequestInit): Promise<Blob> {
  const res = await request(path, init);
  return await res.blob();
}

/**
 * A route that answers as a stream (server-sent events).
 *
 * The caller reads `res.body` itself. A 204 is returned, not thrown: it is how
 * the daemon says "nothing to stream", and the caller decides what that means.
 */
export async function apiStream(path: string, init?: RequestInit): Promise<Response> {
  return await request(path, init);
}

/**
 * `GET <base>/health` through the active transport, without any credential.
 * Under the desktop transport this is the bare `fetch(url)` it always was.
 */
function probeFetch(): Promise<Response> {
  const transport = getTransport();
  const url = `${transport.baseUrl}/health`;
  if (transport.probeHeaders === undefined) return fetch(url);
  return fetch(url, { credentials: transport.credentials, headers: transport.probeHeaders });
}

/**
 * Is the daemon there?
 *
 * The one call that carries no token — it is what the shell asks *before* it
 * has a credential, so it cannot need one. `res.ok` is the entire answer, and
 * a thrown fetch is a `false` rather than an exception because "not there" is
 * the expected reading, not an error: the daemon is allowed to be starting up.
 */
export async function probeHealth(): Promise<boolean> {
  try {
    const res = await probeFetch();
    return res.ok;
  } catch {
    return false;
  }
}

/**
 * What `GET /system/update-readiness` answers: the daemon's version, how many
 * runs are live (`running` or `awaiting_approval`), and whether it is already
 * draining for an update. The updater waits for `live_runs` to reach zero.
 */
export interface UpdateReadiness {
  version: string;
  live_runs: number;
  draining: boolean;
}

/**
 * Which version is the daemon?
 *
 * Read from the `X-Nucleos-Version` header of `/health`, which keeps its plain
 * `ok` body for the callers that read it. Like {@link probeHealth} it carries
 * no token and turns "not there" into a value rather than an exception: `null`
 * means the daemon is silent or too old to name itself, and the shell treats
 * both as "not the version I was built with".
 */
export async function probeDaemonVersion(): Promise<string | null> {
  try {
    const res = await probeFetch();
    if (!res.ok) return null;
    return res.headers.get("x-nucleos-version");
  } catch {
    return null;
  }
}
