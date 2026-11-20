import { useQuery } from "@tanstack/react-query";
import { apiFetch } from "./client";
import { keys } from "./keys";
import type { AgentEngine } from "./agents";
import type { AssistantModels } from "./chats";

/**
 * The models an agent may name, per engine. Blank is always allowed and means the engine's
 * default.
 *
 * The two CLI engines have no way to list their own models, so their short names are written
 * once, here, and both the agent page and the New agent dialog read them. `local` has no fixed
 * catalogue: its models are whatever the machine runs, which the daemon lists on
 * `GET /assistant/models` (the `local` brain), so that list is read only for that engine.
 */
export const ENGINE_MODELS: Record<Exclude<AgentEngine, "local">, readonly string[]> = {
  claude: ["opus", "sonnet", "haiku"],
  codex: ["gpt-5-codex", "gpt-5"],
};

/** The model ids the daemon lists for the local brain, empty until it answers. */
export function useLocalModels(enabled: boolean): string[] {
  const models = useQuery({
    queryKey: keys.chats.models,
    queryFn: () => apiFetch<AssistantModels>("/assistant/models"),
    staleTime: 60 * 60 * 1000,
    enabled,
  });
  return (models.data?.choices ?? []).filter((choice) => choice.brain === "local").map((choice) => choice.id);
}

/**
 * PURE: the ids the model select offers for an engine. The agent's current model is kept as an
 * extra option when the list does not hold it, so opening an agent never drops what it names.
 */
export function modelOptions(engine: AgentEngine, current: string, local: readonly string[]): string[] {
  const listed = engine === "local" ? [...local] : [...ENGINE_MODELS[engine]];
  const held = current.trim();
  return held === "" || listed.includes(held) ? listed : [...listed, held];
}
