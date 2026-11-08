/** The name the owner knows an engine (or its provider) by: one map, used by every label here. */
const ENGINE_NAMES: Record<string, string> = {
  claude: "Claude",
  anthropic: "Claude",
  codex: "Codex",
  openai: "Codex",
};

/** "Claude" for claude/anthropic, "Codex" for codex/openai, else the id with a capital. */
export function engineName(id: string): string {
  const known = ENGINE_NAMES[id.toLowerCase()];
  if (known !== undefined) return known;
  return id === "" ? id : id.charAt(0).toUpperCase() + id.slice(1);
}

/** A free account name for an engine: `claude`, then `claude-2`, `claude-3`, ... */
export function suggestLabel(engine: string, taken: readonly string[]): string {
  const used = new Set(taken.map((t) => t.trim().toLowerCase()));
  if (!used.has(engine.toLowerCase())) return engine;
  for (let n = 2; ; n += 1) {
    const candidate = `${engine}-${n}`;
    if (!used.has(candidate.toLowerCase())) return candidate;
  }
}
