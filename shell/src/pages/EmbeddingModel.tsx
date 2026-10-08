import { useState } from "react";
import { isApiRefusal } from "../data/client";
import { useEmbeddingModel, useSetEmbeddingModel } from "../data/embedding";
import { Button, ErrorNote, RefusalNote } from "../ui";

/**
 * The Ollama model that embeds knowledge rows, as a labelled text field under the distiller's.
 *
 * A free name rather than a menu: any embedding model Ollama has pulled will do. The field shows
 * the daemon's stored model until the owner types; Save is offered only for a different name.
 */
export function EmbeddingModel() {
  const stored = useEmbeddingModel();
  const set = useSetEmbeddingModel();
  const [draft, setDraft] = useState<string | null>(null);

  const current = stored.data ?? "";
  const shown = draft ?? current;
  const changed = draft !== null && draft.trim() !== "" && draft.trim() !== current;
  const refusal = [set.error, stored.error].find(isApiRefusal);

  return (
    <div className="sy-field">
      <label htmlFor="sy-embedding-model">Embedding model</label>
      <input
        id="sy-embedding-model"
        className="sy-field-input"
        type="text"
        value={shown}
        disabled={set.isPending}
        onChange={(event) => setDraft(event.target.value)}
      />
      <Button
        disabled={!changed || set.isPending}
        onClick={() => set.mutate((draft ?? "").trim(), { onSuccess: () => setDraft(null) })}
      >
        Save
      </Button>
      <p className="sy-field-hint">
        An Ollama model that is already pulled. Applied at once; rows embedded by the previous model
        are re-embedded in the background.
      </p>
      {refusal ? (
        <RefusalNote refusal={refusal} />
      ) : set.error ? (
        <ErrorNote>the núcleo did not answer — the embedding model was not changed</ErrorNote>
      ) : null}
    </div>
  );
}
