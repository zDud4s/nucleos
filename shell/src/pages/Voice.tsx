// §spec novo-frontend

import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { finishCapture, startCapture, type ActiveCapture } from "../lib/capture";
import { isApiRefusal, type ApiRefusal } from "../data/client";
import {
  phaseAfter,
  postCapture,
  useDeleteMemo,
  useDictations,
  useMemos,
  useVoiceConfig,
  type Capture,
  type CaptureResult,
  type VoiceConfigView,
  type VoicePhase,
} from "../data/voice";
import {
  Badge,
  Button,
  ConfirmButton,
  ErrorNote,
  PageHeader,
  Panel,
  RefusalNote,
  RelativeTime,
  StaleNote,
  StateBadge,
  Teach,
} from "../ui";
import "./voice.css";

/**
 * Voice — configuration, the capture pipeline, memos and dictations.
 *
 * The two facts from `data/voice.ts`'s header that shape everything below:
 * **`armed` is config, not hardware** — the Teach in {@link CaptureButtons}
 * replaces the capture buttons whenever it is false, and says so rather than
 * offering a button that would 503. **A `204` from `POST /voice/capture` is
 * "nothing was heard", not success** — {@link CaptureOutcomeNote} names it
 * explicitly rather than rendering the silence of a successful mutation.
 *
 * **The split this page holds to throughout**: the WEBVIEW owns the
 * microphone and the POST — `beginRecording` / `endRecordingAndCapture`
 * below, using `lib/audio.ts`'s pure conversions — and the HOST owns the
 * tray icon and the paste, which is why a finished dictation's text crosses
 * back out through `invoke("voice_paste", …)` rather than being typed by the
 * webview itself. Audio bytes cannot cross that boundary the other
 * direction either: Tauri IPC serialises arguments as JSON, so recording
 * happens here and only the already-encoded bytes ever leave via `fetch`.
 *
 * Two Tauri surfaces meet here. `voice://start` / `voice://stop` are the
 * REAL system hotkey, fired by the host whether or not this window has
 * focus — this page merely reacts. `invoke("voice_hotkey", { memo })` is
 * this page's own capture buttons standing in for that same hotkey while the
 * window IS focused; its resolved phase decides locally whether to start
 * recording or to stop and post, via {@link phaseAfter}, the one place that
 * decision is made.
 */

/** One derived sentence for the page header. */
function headline(
  config: VoiceConfigView | undefined,
  memos: Capture[] | undefined,
  dictations: Capture[] | undefined,
): string | undefined {
  if (config === undefined) return undefined;
  if (!config.armed) return "not armed — no transcriber is configured";
  if (memos === undefined || dictations === undefined) return "armed";
  const memoNoun = memos.length === 1 ? "memo" : "memos";
  const dictationNoun = dictations.length === 1 ? "dictation" : "dictations";
  return `armed — ${memos.length} ${memoNoun}, ${dictations.length} ${dictationNoun}`;
}

/** The daemon's own sentence, when it really sent one — the pattern `Web.tsx` and `Contacts.tsx` share. */
function daemonProse(refusal: ApiRefusal): Record<string, string> {
  const detail = refusal.detail.trim();
  if (detail === "" || detail === refusal.code) return {};
  if (detail.split(/\s+/).length < 4) return {};
  return { [refusal.code]: detail };
}

/* ------------------------------------------------------------ recording -- */

/** What one in-progress recording is holding, kept in a ref rather than state — none of it should cause a render. */
/** The open microphone, plus the one thing this page needs to remember about it: which list it is for. */
type ActiveRecording = ActiveCapture & { kind: "dictation" | "memo" };

/** What a finished attempt at a capture came back as — read once, shown once, replaced by the next attempt. */
type CaptureOutcome =
  | { kind: "silent" }
  | { kind: "done"; result: CaptureResult }
  | { kind: "refused"; error: unknown }
  | { kind: "mic-error" };

type Delivery = { pasted: boolean; held: string | null };

export function Voice() {
  const config = useVoiceConfig();
  const memos = useMemos();
  const dictations = useDictations();
  const deleteMemo = useDeleteMemo();

  const [phase, setPhase] = useState<VoicePhase>("idle");
  const [activeKind, setActiveKind] = useState<"dictation" | "memo" | null>(null);
  const [pending, setPending] = useState(false);
  const [outcome, setOutcome] = useState<CaptureOutcome | null>(null);
  const [delivery, setDelivery] = useState<Delivery | null>(null);
  const [hotkeyConflicts, setHotkeyConflicts] = useState<string[] | null>(null);
  const [hotkeyRegisterFailed, setHotkeyRegisterFailed] = useState(false);

  const captureRef = useRef<ActiveRecording | null>(null);
  const registeredHotkeysRef = useRef<string | null>(null);

  /** The phase this window already holds, in case the host is mid-capture from before this page mounted. */
  useEffect(() => {
    let cancelled = false;
    invoke<VoicePhase>("voice_phase")
      .then((initial) => {
        if (!cancelled) setPhase(initial);
      })
      .catch(() => {
        // No reading is not a reason to invent one — the button state below
        // already defaults to idle, which is the safe assumption.
      });
    return () => {
      cancelled = true;
    };
  }, []);

  /**
   * Register all three hotkeys once the config that names them has answered, and again only if the
   * chords actually change.
   *
   * All three in ONE call, because `register_hotkeys` unregisters everything before it registers
   * anything — a second call naming only the conversation chord would silently drop the other two.
   * That is also why the conversation chord is registered from this page even though the mode it
   * toggles is driven from the chat: the registration is indivisible, and this is the page that
   * already holds the config it comes from.
   */
  useEffect(() => {
    const dictationHotkey = config.data?.hotkey;
    const memoHotkey = config.data?.memo_hotkey;
    const conversationHotkey = config.data?.conversation_hotkey;
    if (dictationHotkey === undefined || memoHotkey === undefined) return;
    const conversation = conversationHotkey ?? "";
    const key = `${dictationHotkey} ${memoHotkey} ${conversation}`;
    if (registeredHotkeysRef.current === key) return;
    registeredHotkeysRef.current = key;
    invoke<string[]>("voice_register_hotkeys", {
      dictation: dictationHotkey,
      memo: memoHotkey,
      conversation,
    })
      .then((failed) => setHotkeyConflicts(failed))
      .catch(() => setHotkeyRegisterFailed(true));
  }, [config.data?.hotkey, config.data?.memo_hotkey, config.data?.conversation_hotkey]);

  async function beginRecording(kind: "dictation" | "memo") {
    setOutcome(null);
    setDelivery(null);
    try {
      captureRef.current = { ...(await startCapture()), kind };
      setActiveKind(kind);
      setPhase(phaseAfter({ type: "start", kind }));
    } catch {
      setOutcome({ kind: "mic-error" });
    }
  }

  async function endRecordingAndCapture() {
    const active = captureRef.current;
    if (active === null) return;
    captureRef.current = null;
    setPhase(phaseAfter({ type: "stop" }));

    const { bytes, ms } = await finishCapture(active);

    try {
      const result = await postCapture(bytes, active.kind, ms);
      if (result === undefined) {
        // The one route in this shell where a success-family status is a
        // negative answer — see `data/voice.ts`'s header.
        setOutcome({ kind: "silent" });
      } else {
        setOutcome({ kind: "done", result });
        if (active.kind === "dictation") {
          await deliverPaste(result.text);
        }
      }
      setPhase(phaseAfter({ type: "capture-done" }));
    } catch (error) {
      setOutcome({ kind: "refused", error });
      setPhase(phaseAfter({ type: "capture-error" }));
    } finally {
      setActiveKind(null);
    }
  }

  /** The host owns the paste — this only hands the finished text across and reads back whether it landed. */
  async function deliverPaste(text: string) {
    try {
      const result = await invoke<Delivery>("voice_paste", { text });
      setDelivery(result);
    } catch {
      // The host did not answer at all — distinct from `held`, which is the
      // host answering with a named reason. Neither is one of the five
      // sentences `held` carries, so this is not shown as one.
      setDelivery(null);
    }
  }

  async function handleAbandon() {
    const active = captureRef.current;
    captureRef.current = null;
    if (active !== null) {
      active.processor.disconnect();
      active.source.disconnect();
      active.sink.disconnect();
      for (const track of active.stream.getTracks()) track.stop();
      await active.context.close();
    }
    try {
      await invoke("voice_abandon");
    } catch {
      // Best-effort — the host may already have nothing to abandon either.
    }
    setOutcome(null);
    setDelivery(null);
    setActiveKind(null);
    setPhase(phaseAfter({ type: "abandon" }));
  }

  /** The page's own capture buttons, standing in for the hotkey while this window has focus. */
  async function handleHotkeyPress(memo: boolean) {
    if (pending) return;
    setPending(true);
    try {
      const result = await invoke<"recording" | "transcribing" | "busy">("voice_hotkey", { memo });
      if (result === "recording") {
        await beginRecording(memo ? "memo" : "dictation");
      } else if (result === "transcribing") {
        await endRecordingAndCapture();
      } else {
        setPhase(phaseAfter({ type: "hotkey", phase: "busy" }));
      }
    } catch (error) {
      setOutcome({ kind: "refused", error });
    } finally {
      setPending(false);
    }
  }

  /** The real hotkey — fired by the host whether or not this window is focused. */
  useEffect(() => {
    let unlistenStart: (() => void) | undefined;
    let unlistenStop: (() => void) | undefined;

    void listen<"dictation" | "memo">("voice://start", (event) => {
      setActiveKind(event.payload);
      void beginRecording(event.payload);
    }).then((fn) => {
      unlistenStart = fn;
    });

    void listen("voice://stop", () => {
      void endRecordingAndCapture();
    }).then((fn) => {
      unlistenStop = fn;
    });

    return () => {
      unlistenStart?.();
      unlistenStop?.();
    };
    // Registered once: both handlers close only over refs and setState
    // setters, which are stable across renders.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  return (
    <>
      <PageHeader title="Voice" headline={headline(config.data, memos.data, dictations.data)} />

      <Panel title="Capture" aside={<PhaseBadge phase={phase} />}>
        <HotkeyConflictNote failed={hotkeyConflicts} registerFailed={hotkeyRegisterFailed} />
        <CaptureButtons
          phase={phase}
          armed={config.data?.armed ?? false}
          activeKind={activeKind}
          pending={pending}
          onPress={handleHotkeyPress}
          onAbandon={handleAbandon}
        />
        <CaptureOutcomeNote outcome={outcome} />
        <DeliveryNote delivery={delivery} />
      </Panel>

      <ConfigReadout config={config} />

      <MemoList memos={memos} deleteMemo={deleteMemo} />

      <DictationList dictations={dictations} />
    </>
  );
}

/* ---------------------------------------------------------- phase badge -- */

/**
 * The capture phase, local to this page — not a `StateBadge` domain, because
 * `ui/state-map.ts` covers only states verified against the núcleo's own
 * enums, and this one is a fact this page's reducer invents, not one the
 * daemon sends over the wire.
 */
function PhaseBadge({ phase }: { phase: VoicePhase }) {
  if (phase === "idle") return <Badge tone="off">idle</Badge>;
  if (phase === "recording") return <Badge tone="pending">recording</Badge>;
  return <Badge tone="info">transcribing</Badge>;
}

/* ------------------------------------------------------------- capture -- */

function CaptureButtons({
  phase,
  armed,
  activeKind,
  pending,
  onPress,
  onAbandon,
}: {
  phase: VoicePhase;
  armed: boolean;
  activeKind: "dictation" | "memo" | null;
  pending: boolean;
  onPress: (memo: boolean) => void;
  onAbandon: () => void;
}) {
  if (!armed) {
    return (
      <Teach title="Voice is not armed">
        <p>
          Armed means a transcriber is configured on this machine —{" "}
          <code>armed = enabled &amp;&amp; stt_command != ""</code>. It reflects configuration, not
          whether a GPU or a model is actually present: the shell is set up to try, which is not the
          same fact as a capture succeeding.
        </p>
        <p>
          Configuration for this pillar lives in <code>.ai/voice.yaml</code>.
        </p>
      </Teach>
    );
  }

  if (phase === "idle") {
    return (
      <div className="voice-capture-buttons">
        <Button intent="go" disabled={pending} onClick={() => onPress(false)}>
          Start dictation
        </Button>
        <Button intent="go" disabled={pending} onClick={() => onPress(true)}>
          Start memo
        </Button>
      </div>
    );
  }

  return (
    <div className="voice-capture-buttons">
      <Button
        intent="stop"
        disabled={pending || phase === "transcribing"}
        onClick={() => onPress(activeKind === "memo")}
      >
        {phase === "recording" ? `Stop ${activeKind ?? "capture"}` : "Transcribing…"}
      </Button>
      <Button disabled={pending} onClick={onAbandon}>
        Abandon
      </Button>
    </div>
  );
}

function CaptureOutcomeNote({ outcome }: { outcome: CaptureOutcome | null }) {
  if (outcome === null) return null;
  if (outcome.kind === "silent") {
    return (
      <p className="voice-outcome" role="status">
        nothing was heard — check the microphone is not muted and the right input device is selected
      </p>
    );
  }
  if (outcome.kind === "done") {
    return (
      <p className="voice-outcome" role="status">
        captured — <StateBadge domain="voice_cleanup" state={outcome.result.state} />
      </p>
    );
  }
  if (outcome.kind === "mic-error") {
    return <ErrorNote>the microphone could not be opened</ErrorNote>;
  }
  return <CaptureRefusal error={outcome.error} />;
}

function CaptureRefusal({ error }: { error: unknown }) {
  if (isApiRefusal(error)) return <RefusalNote refusal={error} sentences={daemonProse(error)} />;
  return <ErrorNote>the núcleo did not answer — this capture was not sent</ErrorNote>;
}

/** `held` is one of exactly five sentences the host sends verbatim — rendered as-is, never paraphrased. */
function DeliveryNote({ delivery }: { delivery: Delivery | null }) {
  if (delivery === null) return null;
  if (delivery.pasted) {
    return (
      <p className="voice-delivery" role="status">
        pasted into the window you dictated into
      </p>
    );
  }
  return (
    <p className="voice-delivery voice-delivery-held" role="status">
      {delivery.held ?? "the dictation could not be delivered"}
    </p>
  );
}

function HotkeyConflictNote({ failed, registerFailed }: { failed: string[] | null; registerFailed: boolean }) {
  if (registerFailed) {
    return <ErrorNote>the hotkeys could not be registered — the host did not answer</ErrorNote>;
  }
  if (failed === null || failed.length === 0) return null;
  const noun = failed.length === 1 ? "this hotkey" : "these hotkeys";
  return (
    <p className="voice-hotkey-conflict" role="alert">
      {noun} could not be registered — something else on this machine already holds{" "}
      {failed.length === 1 ? "it" : "them"}: {failed.join(", ")}
    </p>
  );
}

/* ---------------------------------------------------------- configuration -- */

function ConfigReadout({ config }: { config: ReturnType<typeof useVoiceConfig> }) {
  const data = config.data;
  return (
    <Panel title="Configuration" variant="dim">
      {data === undefined && config.isError && <ConfigError error={config.error} />}
      {data === undefined && !config.isError && <p className="voice-loading">reading the configuration…</p>}
      {data !== undefined && (
        <>
          <dl className="voice-config">
            <div className="voice-config-fact">
              <dt>state</dt>
              <dd>{data.armed ? "armed — a transcriber is configured" : "not armed — no transcriber is configured"}</dd>
            </div>
            <div className="voice-config-fact">
              <dt>dictation hotkey</dt>
              <dd>{data.hotkey === "" ? "not configured" : data.hotkey}</dd>
            </div>
            <div className="voice-config-fact">
              <dt>memo hotkey</dt>
              <dd>{data.memo_hotkey === "" ? "not configured" : data.memo_hotkey}</dd>
            </div>
            <div className="voice-config-fact">
              <dt>cleanup model</dt>
              <dd>{data.cleanup_model ?? "none — every capture comes back raw"}</dd>
            </div>
            <div className="voice-config-fact">
              <dt>dictation retention</dt>
              <dd>{data.retain_dictations_days} days</dd>
            </div>
            <div className="voice-config-fact">
              <dt>capture ceiling</dt>
              <dd>
                {data.max_capture_seconds}s, {formatBytes(data.max_body_bytes)}
              </dd>
            </div>
          </dl>
          <p className="voice-config-prompt-label">cleanup prompt</p>
          <p className="voice-config-prompt">{data.cleanup_prompt}</p>
          {data.hints.length > 0 && (
            <ul className="voice-hints">
              {data.hints.map((hint) => (
                <li key={hint}>{hint}</li>
              ))}
            </ul>
          )}
          <p className="voice-config-source">
            Read from <code>.ai/voice.yaml</code> — the prompt above is the literal text this pillar
            uses, not a path to the file it comes from.
          </p>
        </>
      )}
    </Panel>
  );
}

function formatBytes(n: number): string {
  return `${(n / (1024 * 1024)).toFixed(1)} MB`;
}

function ConfigError({ error }: { error: unknown }) {
  if (isApiRefusal(error)) return <RefusalNote refusal={error} />;
  return <ErrorNote>the núcleo did not answer — nothing is known about the voice configuration</ErrorNote>;
}

/* ------------------------------------------------------------------ lists -- */

function Count({ n }: { n: number | undefined }) {
  if (n === undefined) return null;
  return <span className="voice-count">{n}</span>;
}

function MemoList({
  memos,
  deleteMemo,
}: {
  memos: ReturnType<typeof useMemos>;
  deleteMemo: ReturnType<typeof useDeleteMemo>;
}) {
  const rows = memos.data;
  const stale = memos.isError && rows !== undefined;

  return (
    <Panel title="Memos" aside={<Count n={rows?.length} />}>
      {stale && <StaleNote dataUpdatedAt={memos.dataUpdatedAt} />}
      {memos.isError && rows === undefined && <MemosError error={memos.error} />}
      {rows === undefined && !memos.isError && <p className="voice-loading">reading the memos…</p>}
      {rows !== undefined && rows.length === 0 && (
        <Teach title="No memos yet">
          <p>A memo is a capture that stays a note — start one above, or press the memo hotkey from anywhere.</p>
        </Teach>
      )}
      {rows !== undefined && rows.length > 0 && (
        <ul className="voice-list" aria-label="Memos">
          {rows.map((row) => (
            <CaptureRow
              key={row.id}
              row={row}
              onDelete={() => deleteMemo.mutate(row.id)}
              deleting={deleteMemo.isPending}
            />
          ))}
        </ul>
      )}
      {deleteMemo.isError && <DeleteMemoError error={deleteMemo.error} />}
    </Panel>
  );
}

function MemosError({ error }: { error: unknown }) {
  if (isApiRefusal(error)) return <RefusalNote refusal={error} />;
  return <ErrorNote>the núcleo did not answer — nothing is known about the memos</ErrorNote>;
}

function DeleteMemoError({ error }: { error: unknown }) {
  if (isApiRefusal(error)) return <RefusalNote refusal={error} />;
  return <ErrorNote>the núcleo did not answer — that memo was not deleted</ErrorNote>;
}

function DictationList({ dictations }: { dictations: ReturnType<typeof useDictations> }) {
  const rows = dictations.data;
  const stale = dictations.isError && rows !== undefined;

  return (
    <Panel title="Dictations" variant="dim" aside={<Count n={rows?.length} />}>
      <p className="voice-note">
        Read by hand for prompt tuning, not by the shell — there is no delete here; dictations expire
        on their own by retention.
      </p>
      {stale && <StaleNote dataUpdatedAt={dictations.dataUpdatedAt} />}
      {dictations.isError && rows === undefined && <DictationsError error={dictations.error} />}
      {rows === undefined && !dictations.isError && <p className="voice-loading">reading the dictations…</p>}
      {rows !== undefined && rows.length === 0 && (
        <Teach title="No dictations yet">
          <p>
            A dictation is a capture that gets pasted where you were typing — start one above, or press
            the dictation hotkey from anywhere.
          </p>
        </Teach>
      )}
      {rows !== undefined && rows.length > 0 && (
        <ul className="voice-list" aria-label="Dictations">
          {rows.map((row) => (
            <CaptureRow key={row.id} row={row} />
          ))}
        </ul>
      )}
    </Panel>
  );
}

function DictationsError({ error }: { error: unknown }) {
  if (isApiRefusal(error)) return <RefusalNote refusal={error} />;
  return <ErrorNote>the núcleo did not answer — nothing is known about the dictations</ErrorNote>;
}

function CaptureRow({ row, onDelete, deleting }: { row: Capture; onDelete?: () => void; deleting?: boolean }) {
  return (
    <li className="voice-row">
      <div className="voice-row-head">
        <StateBadge domain="voice_cleanup" state={row.cleanup_state} />
        <RelativeTime at={row.created_at} />
        <span className="voice-row-duration">{(row.duration_ms / 1000).toFixed(1)}s</span>
        {row.model !== null && <span className="voice-row-model">{row.model}</span>}
      </div>
      {/* `clean_text` is non-null only when `cleanup_state === "cleaned"` — the
          "as heard" text beside it is what §6.15 asks for. */}
      {row.cleanup_state === "cleaned" && row.clean_text !== null ? (
        <>
          <p className="voice-row-clean">{row.clean_text}</p>
          <p className="voice-row-raw">as heard: {row.raw_text}</p>
        </>
      ) : (
        <p className="voice-row-clean">{row.raw_text}</p>
      )}
      {onDelete !== undefined && (
        <ConfirmButton
          label="Delete"
          confirmLabel="Delete this memo"
          variant="danger"
          disabled={deleting}
          onConfirm={onDelete}
        />
      )}
    </li>
  );
}
