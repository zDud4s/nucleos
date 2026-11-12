// Kept apart from System.tsx so the router can validate without importing the lazy page.

/** A channel id, the grammar of §4: a lowercase letter, then up to 31 of `[a-z0-9-]`. */
const CHANNEL_ID = /^[a-z][a-z0-9-]{0,31}$/;

/**
 * The search params `/system/$view` understands: only `channel`, and only on the notifications tab.
 *
 * Anything that is not a well-formed channel id (empty, wrong type, odd characters) is dropped, so
 * the page falls back to the Default scope instead of asking the daemon for a malformed channel.
 */
export function validateSystemSearch(search: Record<string, unknown>): { channel?: string } {
  const raw = search.channel;
  return typeof raw === "string" && CHANNEL_ID.test(raw) ? { channel: raw } : {};
}
