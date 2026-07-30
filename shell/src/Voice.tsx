import { useCallback, useEffect, useState } from "react";
import {
  deleteVoiceMemo, getVoiceConfig, listVoiceMemos,
  type ConnectionState, type VoiceCapture, type VoiceConfigView,
} from "./api";
import { relativeTime, spokenDuration, voiceCleanupLabel, voiceCleanupTone } from "./derive";
import { Badge, ConfirmButton, ErrorNote, Panel } from "./ui";

/**
 * What is in force, read from the daemon and owned by it.
 *
 * There is deliberately nothing editable here. `.ai/voice.yaml` names a program the daemon will
 * spawn, which is why `classifier.rs` lists it as self-governing: an edit to that file is an edit to
 * what this machine will execute, and it belongs behind the same approval as any other such change
 * rather than behind a text box in a window that is already authenticated.
 */
function Configuration({ config }: { config: VoiceConfigView }) {
  return (
    <Panel
      title="Configuration"
      aside={<Badge tone={config.armed ? "active" : "off"}>{config.armed ? "armed" : "off"}</Badge>}
    >
      {!config.armed && (
        <p className="a-note">
          Voice is off. It arms when <code>.ai/voice.yaml</code> sets <code>enabled: true</code> AND
          names an <code>stt_command</code> — enabling it without a transcriber stays off on purpose,
          because a hotkey that records into nothing is worse than one that never fires.
        </p>
      )}
      <dl className="v-kv">
        <dt>Cleanup model</dt>
        <dd>{config.cleanup_model ?? "none — transcripts are kept as spoken"}</dd>
        <dt>Dictation retention</dt>
        <dd>{config.retain_dictations_days} days</dd>
        <dt>Longest capture</dt>
        <dd>{spokenDuration(config.max_capture_seconds * 1000)}</dd>
        <dt>Misheard-word hints</dt>
        <dd>{config.hints.length === 0 ? "none" : config.hints.join(", ")}</dd>
      </dl>
      <p className="a-note">
        Memos are kept until you delete them; dictations expire on the retention above. Audio is never
        stored at all — the recording is deleted as soon as it has been transcribed, including when the
        request that carried it is abandoned.
      </p>
      <details>
        <summary>Cleanup prompt</summary>
        <pre className="v-prompt">{config.cleanup_prompt}</pre>
      </details>
    </Panel>
  );
}

/**
 * The memos, newest first.
 *
 * `raw_text` is shown whenever there is no cleaned text, rather than showing nothing: a refused or
 * unattempted cleanup still leaves a complete transcript, and hiding it would lose the one thing the
 * capture was for.
 */
function Memos({
  memos, busy, onDelete,
}: {
  memos: VoiceCapture[];
  busy: number | null;
  onDelete: (id: number) => void;
}) {
  if (memos.length === 0) {
    return (
      <Panel title="Memos">
        <p className="a-note">
          No memos yet. A memo is a long dictation kept as a document — it survives the client
          disconnecting mid-transcription, unlike a quick dictation, which is discarded with its
          request.
        </p>
      </Panel>
    );
  }

  return (
    <Panel title="Memos" aside={<span className="v-count">{memos.length}</span>}>
      <ul className="v-memos">
        {memos.map((memo) => (
          <li key={memo.id}>
            <header>
              <Badge tone={voiceCleanupTone(memo.cleanup_state)}>
                {voiceCleanupLabel(memo.cleanup_state)}
              </Badge>
              <span className="v-when">{relativeTime(memo.created_at)}</span>
              <span className="v-len">{spokenDuration(memo.duration_ms)}</span>
              {memo.model !== null && <span className="v-model">{memo.model}</span>}
              <ConfirmButton
                variant="danger"
                size="sm"
                confirmLabel="Delete for good?"
                disabled={busy === memo.id}
                onConfirm={() => onDelete(memo.id)}
              >
                Delete
              </ConfirmButton>
            </header>
            <p className="v-text">{memo.clean_text ?? memo.raw_text}</p>
          </li>
        ))}
      </ul>
    </Panel>
  );
}

/**
 * The voice pillar's window: what is configured, and what has been said.
 *
 * It cannot start a recording. The hotkey and the microphone need the shell's Rust side, and that is
 * blocked on `cpal` being incompatible with the `windows` crate version Tauri already requires — so
 * this page reads and manages what the daemon has, and captures arrive through `POST /voice/capture`
 * from whatever does the recording. Saying so here beats a record button that does nothing.
 */
export default function Voice({
  token, connection,
}: {
  token: string | null;
  connection: ConnectionState;
}) {
  const [config, setConfig] = useState<VoiceConfigView | null>(null);
  const [memos, setMemos] = useState<VoiceCapture[] | null>(null);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState<number | null>(null);

  const load = useCallback(async () => {
    if (token === null) return;
    const [nextConfig, nextMemos] = await Promise.all([
      getVoiceConfig(token),
      listVoiceMemos(token),
    ]);
    setConfig(nextConfig);
    setMemos(nextMemos);
    setLoading(false);
  }, [token]);

  useEffect(() => {
    if (token === null || connection !== "connected") return;
    let cancelled = false;
    const tick = async () => {
      await load();
      if (cancelled) return;
    };
    void tick();
    // Captures can land from outside this window — anything holding a token may POST one — so the
    // list is polled rather than only refreshed by our own deletes.
    const id = setInterval(() => void tick(), 5000);
    return () => {
      cancelled = true;
      clearInterval(id);
    };
  }, [connection, load, token]);

  const remove = useCallback(
    (id: number) => {
      if (token === null) return;
      setBusy(id);
      void (async () => {
        await deleteVoiceMemo(token, id);
        // Reload rather than splice: a failed delete and a successful one must not look different,
        // and what the daemon has is the only answer worth drawing.
        await load();
        setBusy(null);
      })();
    },
    [load, token],
  );

  if (token === null) {
    return (
      <section className="page-voice">
        <ErrorNote>The daemon token has not been read yet, so voice cannot be queried.</ErrorNote>
      </section>
    );
  }

  return (
    <section className="page-voice">
      {loading && config === null && <p className="a-note">Reading the voice configuration…</p>}
      {!loading && config === null && (
        <ErrorNote>
          Could not read the voice configuration. The daemon may predate the voice pillar.
        </ErrorNote>
      )}
      {config !== null && <Configuration config={config} />}
      {memos === null
        ? !loading && <ErrorNote>Could not list the memos.</ErrorNote>
        : <Memos memos={memos} busy={busy} onDelete={remove} />}
    </section>
  );
}
