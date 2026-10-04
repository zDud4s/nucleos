// §spec novo-frontend

import { useEffect, useRef, useState } from "react";
import { useNavigate, useSearch } from "@tanstack/react-router";
import { useDictation, type CaptureOutcome, type Delivery } from "../app/Dictation";
import { isApiRefusal, type ApiRefusal } from "../data/client";
import { useVoiceConversation, type ConversationView } from "../data/conversation";
import { useHotkeyRegistration } from "../data/hotkeys";
import { useVoiceChat } from "../data/voice-chat";
import type { SilenceReason } from "../lib/vad";
import {
  useDeleteMemo,
  useDictations,
  useMemos,
  useVoiceConfig,
  type Capture,
  type VoiceConfigView,
  type VoicePhase,
} from "../data/voice";
import {
  Badge,
  Button,
  ConfirmButton,
  Count,
  ErrorNote,
  PageHeader,
  Panel,
  Quiet,
  RefusalNote,
  RelativeTime,
  Row,
  Rows,
  StaleNote,
  StateBadge,
  Teach,
  Section,
} from "../ui";
import { VoiceOrb } from "./VoiceOrb";
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
 * **The capture itself is not this page's**: the microphone, the POST and the
 * paste live in `app/Dictation.tsx`, mounted by the shell on every page, because
 * the chords that drive them are global and a dictation is pasted into whatever
 * application has focus. This page draws that state and presses the same controls.
 */

/** One derived sentence for the page header. */
function headline(
  config: VoiceConfigView | undefined,
  memos: Capture[] | undefined,
  dictations: Capture[] | undefined,
  conflicts: string[] | null,
  registerFailed: boolean,
): string | undefined {
  if (config === undefined) return undefined;
  if (!config.armed) return "not armed — no transcriber is configured";
  if (registerFailed) return "armed, but the hotkeys did not register — use the buttons below";
  if (conflicts !== null && conflicts.length > 0) {
    return `armed — ${conflicts.length} hotkey${conflicts.length === 1 ? "" : "s"} is taken by another app`;
  }
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

export function Voice() {
  const config = useVoiceConfig();
  const memos = useMemos();
  const dictations = useDictations();
  const deleteMemo = useDeleteMemo();

  const { phase, activeKind, pending, outcome, delivery, press, abandon } = useDictation();
  /* Registered by the shell, not here — see `data/hotkeys.ts`. The page reads the outcome of the one
     registration, which the shell has usually finished before anybody opens this page. */
  const hotkeys = useHotkeyRegistration();
  const hotkeyConflicts = hotkeys?.conflicts ?? null;
  const hotkeyRegisterFailed = hotkeys?.failed ?? false;
  const hotkeysUnavailable = hotkeys?.unavailable ?? null;

  return (
    <>
      <PageHeader title="Voice" headline={headline(config.data, memos.data, dictations.data, hotkeyConflicts, hotkeyRegisterFailed)} />

      <Panel title="Capture" aside={<PhaseBadge phase={phase} registerFailed={hotkeyRegisterFailed} />}>
        {hotkeysUnavailable === null ? (
          <HotkeyConflictNote failed={hotkeyConflicts} registerFailed={hotkeyRegisterFailed} />
        ) : (
          // The host's words, not this page's: `dictation.rs` owns the sentence, and a conflict
          // note would be the wrong one anyway — nothing was registered to conflict with.
          <p className="voice-hotkey-conflict" role="status">
            {hotkeysUnavailable}
          </p>
        )}
        <CaptureButtons
          phase={phase}
          armed={config.data?.armed ?? false}
          activeKind={activeKind}
          pending={pending}
          onPress={press}
          onAbandon={abandon}
        />
        <CaptureOutcomeNote outcome={outcome} />
        <DeliveryNote delivery={delivery} />
      </Panel>

      <Conversation armed={config.data?.armed} />

      <ConfigReadout config={config} />

      <MemoList memos={memos} deleteMemo={deleteMemo} />

      <DictationList dictations={dictations} />
    </>
  );
}

/* --------------------------------------------------------- conversation -- */

/**
 * Talking to the agent out loud: it hears a sentence, sends it as a turn, and reads the answer back.
 *
 * **Not the dictation above, and not the microphone in a chat's composer.** Those two turn speech
 * into TEXT and hand it over — a dictation is pasted by the host, and the chat's microphone fills the
 * box you were about to type in. Both leave the words in front of somebody before anything is sent,
 * which is why they are the right shape THERE: a misheard sentence is editable, and the first sentence
 * in a chat is the one that opens a conversation. This is the other thing, and it is here because the
 * owner put it here on 2026-09-19: a spoken turn goes out as it was heard, and the answer comes back
 * out loud.
 *
 * Which conversation it belongs to is `data/voice-chat.ts`'s whole subject — one dedicated chat,
 * reused, because the daemon refuses a turn that names none.
 */
function Conversation({ armed }: { armed: boolean | undefined }) {
  const chat = useVoiceChat();
  const voice = useVoiceConversation(chat.chatId);
  const [opening, setOpening] = useState(false);
  /* The same fact as `opening`, readable inside an await. A chord pressed while the button's own
     request is in flight would otherwise open the conversation twice, and two `POST`s before either
     has settled are two conversations. */
  const openingRef = useRef(false);
  const on = voice.phase !== "off";

  async function press() {
    if (openingRef.current) return;
    if (on) {
      voice.toggle();
      return;
    }
    openingRef.current = true;
    setOpening(true);
    try {
      /* The conversation first, and awaited: a turn that names no chat is refused by the daemon
         outright, in as many words. The id reaches the hook on the render this causes — which is
         many frames before anything can be spoken into a microphone that is not even open yet, so
         the toggle below cannot outrun it. */
      if ((await chat.open()) === null) return;
      voice.toggle();
    } finally {
      openingRef.current = false;
      setOpening(false);
    }
  }

  useChordRequest(armed, press);

  return (
    <Panel title="Conversation" aside={<ConversationBadge phase={voice.phase} />}>
      {armed === true ? (
        <>
          <VoiceOrb phase={voice.phase} level={voice.level} threshold={voice.threshold} />
          <div className="voice-capture-buttons">
            <Button intent={on ? "stop" : "go"} disabled={opening} onClick={() => void press()}>
              {on ? "Stop talking" : "Start talking"}
            </Button>
          </div>
          <ConversationStatus voice={voice} />
          {chat.trouble !== null && <ErrorNote>{chat.trouble}</ErrorNote>}
        </>
      ) : (
        <Quiet says="not armed — nothing here can transcribe a spoken turn" />
      )}
    </Panel>
  );
}

/**
 * The `talk` stamp `app/ConversationChord.tsx` puts in the address when the conversation chord is
 * pressed. A number or nothing: anything else a person typed there is the same as not asking.
 */
export function validateVoiceSearch(search: Record<string, unknown>): { talk?: number } {
  const talk = typeof search.talk === "number" ? search.talk : Number(search.talk);
  return Number.isFinite(talk) && search.talk !== undefined && search.talk !== "" ? { talk } : {};
}

/**
 * Acts on the chord's stamp once, through `press` — the button's own path, so the chord opens the
 * conversation before the microphone exactly as a click does.
 *
 * Waits for the configuration rather than acting on its absence: arriving from another page, the
 * stamp lands before `armed` has answered, and treating "not answered yet" as "not armed" would
 * swallow the one press that brought the person here. Once it has answered, an unarmed page consumes
 * the stamp and does nothing — the panel already says why, in place of the button.
 *
 * The stamp is removed from the address as it is consumed, so a reload or a Back does not start a
 * conversation nobody asked for this time. The consumed value is remembered as well, because clearing
 * the address is itself a navigation and this effect sees the old stamp again before it lands.
 */
function useChordRequest(armed: boolean | undefined, press: () => Promise<void>) {
  const navigate = useNavigate();
  const { talk } = validateVoiceSearch(useSearch({ strict: false }) as Record<string, unknown>);
  const pressRef = useRef(press);
  pressRef.current = press;
  const consumedRef = useRef<number | undefined>(undefined);

  useEffect(() => {
    if (talk === undefined || talk === consumedRef.current || armed === undefined) return;
    consumedRef.current = talk;
    void navigate({ to: "/voice", search: {}, replace: true });
    if (armed) void pressRef.current();
  }, [talk, armed, navigate]);
}

/**
 * What each phase is called, and its one colour.
 *
 * Mapped here rather than in `state-map.ts`, for the reason that file's own docstring gives: it holds
 * the domains the núcleo writes as Rust literals, and `ConversationPhase` is this shell's state
 * machine. {@link PhaseBadge} two screens down is the same shape for the same reason.
 */
function ConversationBadge({ phase }: { phase: ConversationView["phase"] }) {
  if (phase === "off") return <Badge tone="off">off</Badge>;
  if (phase === "listening") return <Badge tone="active">listening</Badge>;
  if (phase === "hearing") return <Badge tone="pending">hearing you</Badge>;
  return <Badge tone="info">{phase === "thinking" ? "thinking" : "answering"}</Badge>;
}

/**
 * Why a listening microphone has opened no turn.
 *
 * `flat` deliberately states a fact and asks nothing. It used to read "the microphone hears nothing —
 * is it muted, or the wrong one?", which is a diagnosis, and it appeared in an ordinary quiet room
 * four seconds after switching on — measured on 2026-09-19. The cause is a measurement floor rather
 * than a bad threshold: `energyOf` bottoms out at -50 dBFS, and a quiet room with a desk microphone
 * sits at about that, so the number genuinely cannot tell a silent room from a dead device. A sentence
 * that claims otherwise is wrong four times out of five; one that reports the silence is right either
 * way, and is still the clue somebody needs when the device really is muted.
 */
const SILENCE_SENTENCES: Record<SilenceReason, string> = {
  noMicrophone: "the microphone is not sending any sound",
  flat: "silence — nothing is reaching the microphone",
  byLoudness: "not loud enough to open a turn — speak up or come closer",
  belowThreshold: "sound, but not speech — noise, or too far from the microphone",
};

/** What the conversation heard, what it is hearing, and anything that stopped it working. */
function ConversationStatus({ voice }: { voice: ConversationView }) {
  const on = voice.phase !== "off";
  const metering =
    voice.phase === "listening" || voice.phase === "hearing" || voice.phase === "speaking";

  if (
    !on &&
    voice.heard === null &&
    voice.trouble === null &&
    voice.assembling === null &&
    voice.ignoredEcho === 0
  ) {
    return null;
  }

  return (
    <div className="voice-conversation-status">
      {/* The level and the bar answer one question together: is it hearing me, and is that enough to
          open a turn? The bar moves on its own — it rises while the answer plays — which is the
          difference between "it is ignoring me" and "it is holding a higher bar for a moment". */}
      {metering && (
        <span
          role="meter"
          aria-label="microphone level"
          aria-valuemin={0}
          aria-valuemax={1}
          aria-valuenow={voice.level}
          aria-valuetext={`level ${Math.round(voice.level * 100)}%, a turn opens at ${Math.round(voice.threshold * 100)}%`}
          className="voice-meter"
        >
          <span
            aria-hidden="true"
            className="voice-meter-fill"
            style={{ width: `${voice.level * 100}%` }}
          />
          <span
            aria-hidden="true"
            className="voice-meter-bar"
            style={{ left: `${voice.threshold * 100}%` }}
          />
        </span>
      )}
      {/* Shown as soon as it is heard and BEFORE the answer, because a misheard question that only
          becomes visible once it has been answered is a question nobody got to correct. */}
      {voice.heard !== null && <span>heard: “{voice.heard}”</span>}
      {voice.assembling !== null && <span>hearing: “{voice.assembling}”</span>}
      {voice.silence !== null && <span>{SILENCE_SENTENCES[voice.silence]}</span>}
      {/* A silently dropped segment looks exactly like a failed microphone, so say when the answer was
          heard again and deliberately not treated as a turn. */}
      {voice.ignoredEcho > 0 && (
        <span title="it heard its own answer through the microphone and did not treat it as a turn">
          ignored its own voice ×{voice.ignoredEcho}
        </span>
      )}
      {on && !voice.hasVoice && <span>no voice on this machine — the answer will be written</span>}
      {voice.listeningWith === "energy" && (
        <span>
          listening by loudness — noise may open a turn
          {voice.whyByLoudness === null ? "" : ` (${voice.whyByLoudness})`}
        </span>
      )}
      {voice.trouble !== null && <ErrorNote>{voice.trouble}</ErrorNote>}
    </div>
  );
}

/* ---------------------------------------------------------- phase badge -- */

/**
 * The capture phase, local to this page — not a `StateBadge` domain, because
 * `ui/state-map.ts` covers only states verified against the núcleo's own
 * enums, and this one is a fact this page's reducer invents, not one the
 * daemon sends over the wire.
 */
function PhaseBadge({ phase, registerFailed }: { phase: VoicePhase; registerFailed: boolean }) {
  // `idle` above an error is two readings of one moment. When the hotkeys did not register,
  // the head says the thing the note underneath is about.
  if (phase === "idle" && registerFailed) return <Badge tone="danger">hotkeys failed</Badge>;
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
          <code>armed = enabled &amp;&amp; (stt_command != "" || stt_url != "")</code>. Either engine
          arms it, since 2026-09-20: a machine pointed at a resident whisper server is the
          configuration that is six times faster, and naming only the command would have reported no
          voice at all on it. It reflects configuration, not
          whether a GPU or a model is actually present: the shell is set up to try, which is not the
          same fact as a capture succeeding.
        </p>
        <p>
          Configuration for this pillar lives in <code>~/.nucleos/voice.yaml</code>.
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
    /**
     * A `204` is the one success-family status in this shell that is a negative
     * answer — see `data/voice.ts`'s header — so it is named rather than left as
     * the silence of a successful mutation. It is an absence and not a fault,
     * which is why it is a `Quiet` rather than an `ErrorNote`: nothing failed,
     * there was simply nothing there. `announce` is the `role="status"` the
     * hand-rolled line carried, and it is right here for the reason the prop
     * exists — this line is the answer to the stop the reader just pressed.
     */
    return (
      <Quiet
        says="nothing was heard — check the microphone is not muted and the right input device is selected"
        announce
      />
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

/** `held` is one of the sentences the host sends verbatim — rendered as-is, never paraphrased. */
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
            Read from <code>~/.nucleos/voice.yaml</code> — the prompt above is the literal text this pillar
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
    <>
      {rows !== undefined && rows.length === 0 ? (
        <Section label="Memos">
          <Quiet says="no memos yet">
            <p>A memo is a capture that stays a note — start one above, or press the memo hotkey from anywhere.</p>
          </Quiet>
        </Section>
      ) : (
    <Panel title="Memos" aside={<Count n={rows?.length} />}>
      {stale && <StaleNote dataUpdatedAt={memos.dataUpdatedAt} />}
      {memos.isError && rows === undefined && <MemosError error={memos.error} />}
      {rows === undefined && !memos.isError && <p className="voice-loading">reading the memos…</p>}
      {rows !== undefined && rows.length > 0 && (
        <Rows label="Memos">
          {rows.map((row) => (
            <CaptureRow
              key={row.id}
              row={row}
              onDelete={() => deleteMemo.mutate(row.id)}
              deleting={deleteMemo.isPending}
            />
          ))}
        </Rows>
      )}
      {deleteMemo.isError && <DeleteMemoError error={deleteMemo.error} />}
    </Panel>
      )}
    </>
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
    <>
      {rows !== undefined && rows.length === 0 ? (
        <Section label="Dictations">
          <Quiet says="no dictations yet">
            <p className="voice-note">A dictation is a capture that gets pasted where you were typing — start one above, or press the dictation hotkey from anywhere.</p>
          </Quiet>
        </Section>
      ) : (
    <Panel title="Dictations" variant="dim" aside={<Count n={rows?.length} />}>
      {stale && <StaleNote dataUpdatedAt={dictations.dataUpdatedAt} />}
      {dictations.isError && rows === undefined && <DictationsError error={dictations.error} />}
      {rows === undefined && !dictations.isError && <p className="voice-loading">reading the dictations…</p>}
      {rows !== undefined && rows.length > 0 && (
        <Rows label="Dictations">
          {rows.map((row) => (
            <CaptureRow key={row.id} row={row} />
          ))}
        </Rows>
      )}
    </Panel>
      )}
    </>
  );
}

function DictationsError({ error }: { error: unknown }) {
  if (isApiRefusal(error)) return <RefusalNote refusal={error} />;
  return <ErrorNote>the núcleo did not answer — nothing is known about the dictations</ErrorNote>;
}

function CaptureRow({ row, onDelete, deleting }: { row: Capture; onDelete?: () => void; deleting?: boolean }) {
  return (
    <Row>
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
    </Row>
  );
}
