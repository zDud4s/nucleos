/**
 * A product name for a vendor model id ("claude-sonnet-5-5" -> "Sonnet 5.5").
 *
 * The daemon is the source of truth: `model_catalog::display_name` labels every model it serves,
 * and both are tested against the same example table. This copy exists only for ids the daemon
 * never labelled (a stale pin, or `configured` when the groups route failed), so keep the rules
 * identical rather than clever.
 */
const ALIASES = new Set(["opus", "sonnet", "haiku", "fable"]);

function title(token: string): string {
  return token.length === 0 ? token : token[0].toUpperCase() + token.slice(1);
}

function titled(tokens: string[]): string {
  return tokens.map(title).join(" ");
}

export function displayName(id: string): string {
  const cleaned = id
    .trim()
    .toLowerCase()
    .replace(/-latest$/, "")
    .replace(/-\d{8}$/, "");
  const tokens = cleaned.split("-").filter((t) => t.length > 0);
  if (tokens.length === 0) return "";

  if (tokens[0] === "claude") {
    const rest = tokens.slice(1);
    const family = rest.find((t) => /^[a-z]+$/.test(t));
    const version = rest.filter((t) => /^\d[\d.]*$/.test(t)).join(".");
    if (family === undefined) return titled(rest);
    return version === "" ? title(family) : `${title(family)} ${version}`;
  }
  if (tokens.length === 1 && ALIASES.has(tokens[0])) return title(tokens[0]);
  if (tokens[0] === "gpt" && tokens.length > 1) {
    const head = `GPT-${tokens[1]}`;
    const rest = tokens.slice(2);
    return rest.length === 0 ? head : `${head} ${titled(rest)}`;
  }
  if (/^o\d/.test(tokens[0])) {
    const rest = tokens.slice(1);
    return rest.length === 0 ? tokens[0] : `${tokens[0]} ${titled(rest)}`;
  }
  return titled(tokens);
}
