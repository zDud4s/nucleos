import { desktopTransport, getTransport } from "../data/transport";

/**
 * Whether this window reaches the núcleo through the remote gateway rather than the desktop host.
 *
 * Read off the transport seam, so desktop code can gate host-only features without importing the
 * browser adapters.
 */
export function isRemote(): boolean {
  return getTransport() !== desktopTransport;
}

/**
 * Whether an error is the browser adapters' "no desktop host behind this tab" rejection.
 * Matched by name: desktop code must not import the adapter that defines the class.
 */
export function isHostOnlyError(error: unknown): boolean {
  return error instanceof Error && error.name === "HostOnlyError";
}
