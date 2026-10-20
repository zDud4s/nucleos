// §spec pilar-de-browser

/** Where free text in the omnibox goes. */
export const SEARCH_ENGINE_URL = "https://duckduckgo.com/?q=";

const EXPLICIT_SCHEME = /^(https?:\/\/|about:)/i;
const LOCALHOST = /^localhost(:\d+)?([/?#].*)?$/i;
const IPV4 = /^\d{1,3}(\.\d{1,3}){3}(:\d+)?([/?#].*)?$/;
const HOST_WITH_DOT = /^[a-z0-9-]+(\.[a-z0-9-]+)+(:\d+)?([/?#].*)?$/i;

/**
 * Turn omnibox input into a URL to load, or null for empty input.
 * Only http(s) and about: are navigated to as written; any other
 * `scheme:` (javascript:, file:, data:, ...) is treated as a search query.
 */
export function resolveOmnibox(input: string): string | null {
  const text = input.trim();
  if (!text) return null;
  if (EXPLICIT_SCHEME.test(text)) return text;
  if (!/\s/.test(text)) {
    if (LOCALHOST.test(text) || IPV4.test(text)) return `http://${text}`;
    if (HOST_WITH_DOT.test(text)) return `https://${text}`;
  }
  return SEARCH_ENGINE_URL + encodeURIComponent(text);
}
