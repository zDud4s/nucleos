// §spec novo-frontend

import { useEffect, useRef, useState, type ReactNode } from "react";
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
  CopyButton,
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
  Tabs,
  TabsContent,
  TabsList,
  TabsTrigger,
  Teach,
  Section,
} from "../ui";
import { VoiceOrb } from "./VoiceOrb";
import { VoiceRoom } from "./VoiceRoom";
import "./voice.css";
import { DAEMON_NAME } from "../copy";

/**
 * Voice — configuration, the capture pipeline, memos and dictations.
 *
 * The two facts from `data/voice.ts`'s header that shape everything below:
 * **`armed` is config, not hardware** — the Teach in {@link NotArmed}
 * stands in the room whenever it is false, and says so rather than
 * offering a button that would 503. **A `204` from `POST /voice/capture` is
 * "nothing was heard", not success** — {@link CaptureOutcomeNote} names it
 * explicitly rather than rendering the silence of a successful mutation.
 *
 * **The capture itself is not this page's**: the microphone, the POST and the
 * paste live in `app/Dictation.tsx`, mounted by the shell on every page, because
 * the chords that drive them are global and a dictation is pasted into whatever
 * application has focus. This page draws that state and presses the same controls.
 *
 * **The page is a room**, full-bleed: the stage and the orb behind, the captions in the middle, a
 * dock along the bottom, and the archive (memos, dictations, configuration) in a drawer.
 *
 * **The spoken conversation is not the dictation.** The dictation and the microphone in a chat's
 * composer turn speech into TEXT and hand it over - a dictation is pasted by the host, and the chat's
 * microphone fills the box you were about to type in. Both leave the words in front of somebody
 * before anything is sent, which is the right shape THERE: a misheard sentence is editable. The
 * conversation is the other thing, and it is here because the owner put it here on 2026-09-19: a
 * spoken turn goes out as it was heard, and the answer comes back out loud. Which conversation it
 * belongs to is `data/voice-chat.ts`'s whole subject - one dedicated chat, reused, because the
 * daemon refuses a turn that names none.
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
  if (!config.armed) return "no transcriber is set up yet";
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

  const { phase, activeKind, pending, outcome, delivery, press: pressCapture, abandon } = useDictation();
  /* Registered by the shell, not here — see `data/hotkeys.ts`. The page reads the outcome of the one
     registration, which the shell has usually finished before anybody opens this page. */
  const hotkeys = useHotkeyRegistration();
  const hotkeyConflicts = hotkeys?.conflicts ?? null;
  const hotkeyRegisterFailed = hotkeys?.failed ?? false;
  const hotkeysUnavailable = hotkeys?.unavailable ?? null;

  const armed = config.data?.armed === true;
  const dictationHotkey = config.data?.hotkey ?? "";
  const memoHotkey = config.data?.memo_hotkey ?? "";
  const conversationHotkey = config.data?.conversation_hotkey ?? "";

  /* The conversation: which chat it speaks into, and the runner that owns the microphone. It lives at
     the page because the room draws it in four places — the stage, the captions, the dock and the
     header's badge — and they all read this one view. */
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

  useChordRequest(config.data?.armed, press);

  /* The archive: the memos, the dictations and the configuration, in a drawer that is not on the page
     until somebody asks for it. */
  const [archiveOpen, setArchiveOpen] = useState(false);
  const archiveButton = useRef<HTMLButtonElement>(null);

  function closeArchive() {
    setArchiveOpen(false);
    // Focus goes back where it came from: a drawer that closes and leaves it on nothing strands
    // anybody navigating by keyboard at the top of the page.
    archiveButton.current?.focus();
  }

  /* The dock grows (stacked columns, notes, outcomes) and the captions and the orb are placed
     against its top edge. Its measured height goes to the room as a CSS variable, so they step
     clear instead of sitting under it; 96px is what the room's geometry assumes. */
  const roomRef = useRef<HTMLDivElement>(null);
  const dockRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const room = roomRef.current;
    const dock = dockRef.current;
    if (room === null || dock === null || typeof ResizeObserver !== "function") return;
    const measure = () => {
      room.style.setProperty("--voice-dock-h", `${Math.max(96, Math.ceil(dock.offsetHeight))}px`);
    };
    const observer = new ResizeObserver(measure);
    observer.observe(dock);
    measure();
    return () => observer.disconnect();
  }, []);

  const memoCount = memos.data?.length;
  const dictationCount = dictations.data?.length;
  const counts = [
    memoCount === undefined ? null : plural(memoCount, "memo"),
    dictationCount === undefined ? null : plural(dictationCount, "dictation"),
  ].filter((part): part is string => part !== null);

  return (
    <div
      className="voice-room"
      ref={roomRef}
      data-phase={voice.phase}
      data-archive={archiveOpen ? "open" : undefined}
      data-live={on || phase !== "idle" ? "" : undefined}
    >
      <VoiceRoom phase={voice.phase} level={voice.level} threshold={voice.threshold} tap={voice.answerTap} />
      <VoiceOrb phase={voice.phase} tap={voice.answerTap} />

      <div className="voice-room-top">
        <PageHeader
          title="Voice"
          headline={headline(config.data, memos.data, dictations.data, hotkeyConflicts, hotkeyRegisterFailed)}
          actions={
            <>
              <ConversationBadge phase={voice.phase} />
              {/* A plain button and not the `Button` primitive: focus has to come back to it, which
                  takes a ref. The counts are spelled out in the label because, drawn beside the word,
                  they would be announced as "Archive2 memos". */}
              <button
                type="button"
                className="ui-button ui-button-ghost voice-archive-open"
                ref={archiveButton}
                aria-expanded={archiveOpen}
                aria-controls="voice-archive"
                aria-label={counts.length === 0 ? "Archive" : `Archive, ${counts.join(" and ")}`}
                onClick={() => (archiveOpen ? closeArchive() : setArchiveOpen(true))}
              >
                Archive
                {counts.length > 0 && (
                  <span className="voice-archive-count" aria-hidden="true">
                    {counts.join(" · ")}
                  </span>
                )}
              </button>
            </>
          }
        />
      </div>

      {armed ? <Captions voice={voice} hotkey={conversationHotkey} /> : <NotArmed />}

      <div className="voice-dock" ref={dockRef}>
        <div className="voice-dock-diag">
          <ConversationStatus voice={voice} />
        </div>

        <div className="voice-dock-talk">
          {armed && (
            <>
              <Button intent={on ? "stop" : "go"} disabled={opening} onClick={() => void press()}>
                {on ? "Stop talking" : "Start talking"}
                {conversationHotkey !== "" && (
                  <kbd aria-hidden="true" className="voice-kbd">
                    {conversationHotkey}
                  </kbd>
                )}
              </Button>
              {chat.trouble !== null && <ErrorNote>{chat.trouble}</ErrorNote>}
            </>
          )}
        </div>

        <div className="voice-dock-capture" role="group" aria-label="Capture">
          <PhaseBadge phase={phase} registerFailed={hotkeyRegisterFailed} />
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
            armed={armed}
            activeKind={activeKind}
            pending={pending}
            hotkey={dictationHotkey}
            memoHotkey={memoHotkey}
            onPress={pressCapture}
            onAbandon={abandon}
          />
          <CaptureOutcomeNote outcome={outcome} />
          <DeliveryNote delivery={delivery} />
        </div>
      </div>

      {archiveOpen && (
        <Archive
          config={config}
          memos={memos}
          dictations={dictations}
          deleteMemo={deleteMemo}
          onClose={closeArchive}
        />
      )}
    </div>
  );
}

/** "2 memos", "1 memo" — the noun agrees with the number. */
function plural(n: number, noun: string): string {
  return `${n} ${n === 1 ? noun : `${noun}s`}`;
}

/* ------------------------------------------------------------ the archive -- */

/**
 * The drawer: what has been captured, and how the pillar is configured.
 *
 * Mounted only while open, so nothing in it is reachable by keyboard when it is shut. It takes focus
 * onto its Close button as it mounts; closing — that button, or Escape pressed inside the drawer — gives
 * focus back to the button that opened it.
 */
function Archive({
  config,
  memos,
  dictations,
  deleteMemo,
  onClose,
}: {
  config: ReturnType<typeof useVoiceConfig>;
  memos: ReturnType<typeof useMemos>;
  dictations: ReturnType<typeof useDictations>;
  deleteMemo: ReturnType<typeof useDeleteMemo>;
  onClose: () => void;
}) {
  const closeButton = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    closeButton.current?.focus();
  }, []);

  return (
    <div
      id="voice-archive"
      className="voice-archive"
      role="dialog"
      aria-modal="false"
      aria-label="Voice archive"
      onKeyDown={(event) => {
        // Escape belongs to this drawer, not to the window: inside an armed or held ConfirmButton it
        // stands that control down and nothing more, so the confirmation is not lost half-way.
        if (event.key !== "Escape" || event.defaultPrevented) return;
        if ((event.target as HTMLElement).closest(".ui-confirm-armed, .ui-confirm-holding") !== null) return;
        onClose();
      }}
    >
      <div className="voice-archive-head">
        <h2>Archive</h2>
        <button type="button" className="ui-button ui-button-ghost" ref={closeButton} onClick={onClose}>
          Close
        </button>
      </div>
      <div className="voice-archive-body">
        <Tabs defaultValue="memos">
          <TabsList aria-label="Archive sections">
            <TabsTrigger value="memos">
              Memos <Count n={memos.data?.length} />
            </TabsTrigger>
            <TabsTrigger value="dictations">
              Dictations <Count n={dictations.data?.length} />
            </TabsTrigger>
            <TabsTrigger value="configuration">Configuration</TabsTrigger>
          </TabsList>
          <TabsContent value="memos">
            <MemoList memos={memos} deleteMemo={deleteMemo} />
          </TabsContent>
          <TabsContent value="dictations">
            <DictationList dictations={dictations} />
          </TabsContent>
          <TabsContent value="configuration">
            <ConfigReadout config={config} />
          </TabsContent>
        </Tabs>
      </div>
    </div>
  );
}

/* ------------------------------------------------------------ the captions -- */

/**
 * What the room says in words: what is being heard, what was heard, what the answer is — and, when
 * there is nothing yet, how to begin.
 *
 * Read from the real view and in the order the person lived it. The label names whose words these
 * are, so a screen reader hears "heard" or "nucleos" before them. The block is a polite live region:
 * the words are read out as they change, not only found by a cursor.
 */
function Captions({ voice, hotkey }: { voice: ConversationView; hotkey: string }) {
  const { phase, heard, assembling, answer } = voice;
  const on = phase !== "off";
  let label: string | null = null;
  let words: ReactNode = null;

  if (phase === "hearing") {
    label = "hearing";
    words = (
      <>
        <span className="voice-caption-assembling">{assembling ?? ""}</span>
        <span className="voice-caption-caret" aria-hidden="true" />
      </>
    );
  } else if (phase === "speaking" && answer !== null && answer !== "") {
    label = "nucleos";
    words = answer;
  } else if (heard !== null) {
    label = !on ? "last heard" : phase === "thinking" ? "heard — sending" : "heard";
    words = `“${heard}”`;
  }

  const quiet =
    !on && heard === null
      ? `press Start talking${hotkey === "" ? "" : `, or ${hotkey} from anywhere,`} and speak`
      : null;

  return (
    <div className="voice-captions" aria-live="polite">
      {label !== null && <span className="voice-caption-label">{label}</span>}
      {words !== null && <p className="voice-caption-words">{words}</p>}
      {quiet !== null && <p className="voice-caption-quiet">{quiet}</p>}
    </div>
  );
}

/** The smallest voice.yaml that turns the pillar on; any speech-to-text program that takes a WAV path works. */
const MINIMAL_VOICE_YAML = "enabled: true\nstt_command: whisper-cli -m model.bin\n";
const VOICE_YAML_PATH = "~/.nucleos/voice.yaml";

/** What stands in the room's middle instead of the captions, on a machine that cannot transcribe. */
function NotArmed() {
  return (
    <div className="voice-notarmed">
      <Quiet says="no transcriber is set up — spoken turns cannot be understood yet" />
      <Teach title="Voice needs a transcriber">
        <p>
          Voice turns your speech into text with a transcriber program that runs on this machine. None is
          set up yet, so the talk and capture buttons are hidden.
        </p>
        <p>
          Add one to <code>{VOICE_YAML_PATH}</code>, then reopen this page. The smallest file that works:
        </p>
        <pre className="voice-notarmed-code">
          <code>{MINIMAL_VOICE_YAML}</code>
        </pre>
        <p className="voice-notarmed-copy">
          <CopyButton value={VOICE_YAML_PATH} label="the voice.yaml path" />
        </p>
        <details className="voice-notarmed-details">
          <summary>Technical details</summary>
          <p>
            Either <code>stt_command</code> (a program that reads a WAV file) or <code>stt_url</code> (a
            resident speech server, which is faster) is enough. This reflects configuration, not whether a
            GPU or a model is actually present: the shell tries, which is not the same as a capture
            succeeding.
          </p>
        </details>
      </Teach>
    </div>
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

/**
 * The dock's diagnostics: the microphone's level against the bar, why nothing opened a turn, and
 * anything that stopped the conversation working. What was heard, and what is being assembled, are
 * the captions' now - said once, where the eye already is.
 */
function ConversationStatus({ voice }: { voice: ConversationView }) {
  const on = voice.phase !== "off";
  const metering =
    voice.phase === "listening" || voice.phase === "hearing" || voice.phase === "speaking";

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
          data-open={voice.level >= voice.threshold ? "" : undefined}
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
      {!on && <span>microphone closed</span>}
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
  hotkey,
  memoHotkey,
  onPress,
  onAbandon,
}: {
  phase: VoicePhase;
  armed: boolean;
  activeKind: "dictation" | "memo" | null;
  pending: boolean;
  hotkey: string;
  memoHotkey: string;
  onPress: (memo: boolean) => void;
  onAbandon: () => void;
}) {
  // Not armed is the room's to say, in the middle of it (`NotArmed`) - a row of buttons here would
  // offer what would 503.
  if (!armed) return null;

  if (phase === "idle") {
    return (
      <>
        <div className="voice-capture-buttons">
          {/* Secondary on purpose: "Start talking" is the room's one primary action. These two are the
              other ways to capture speech, and the hint below says how they differ. */}
          <Button disabled={pending} onClick={() => onPress(false)}>
            Start dictation
            {hotkey !== "" && (
              <kbd aria-hidden="true" className="voice-kbd">
                {hotkey}
              </kbd>
            )}
          </Button>
          <Button disabled={pending} onClick={() => onPress(true)}>
            Start memo
            {memoHotkey !== "" && (
              <kbd aria-hidden="true" className="voice-kbd">
                {memoHotkey}
              </kbd>
            )}
          </Button>
        </div>
        <p className="voice-capture-hint">Dictation is pasted into the app in focus. A memo is saved as a note.</p>
      </>
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
  return <ErrorNote>the {DAEMON_NAME} is not responding — this capture was not sent</ErrorNote>;
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
    return <ErrorNote>the hotkeys could not be registered — the host is not responding</ErrorNote>;
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
  return <ErrorNote>the {DAEMON_NAME} is not responding — nothing is known about the voice configuration</ErrorNote>;
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
            <p>A memo is a capture that stays a note — use Start memo, or press the memo hotkey from anywhere.</p>
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
  return <ErrorNote>the {DAEMON_NAME} is not responding — nothing is known about the memos</ErrorNote>;
}

function DeleteMemoError({ error }: { error: unknown }) {
  if (isApiRefusal(error)) return <RefusalNote refusal={error} />;
  return <ErrorNote>the {DAEMON_NAME} is not responding — that memo was not deleted</ErrorNote>;
}

function DictationList({ dictations }: { dictations: ReturnType<typeof useDictations> }) {
  const rows = dictations.data;
  const stale = dictations.isError && rows !== undefined;

  return (
    <>
      {rows !== undefined && rows.length === 0 ? (
        <Section label="Dictations">
          <Quiet says="no dictations yet">
            <p className="voice-note">A dictation is a capture that gets pasted where you were typing — use Start dictation, or press the dictation hotkey from anywhere.</p>
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
  return <ErrorNote>the {DAEMON_NAME} is not responding — nothing is known about the dictations</ErrorNote>;
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
