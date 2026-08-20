import { keepPreviousData, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { apiFetch } from "./client";
import { keys } from "./keys";
import { POLL } from "./poll";

/**
 * The voice pillar: the pillar's own configuration, the two capture lists it
 * keeps, one delete, and the raw POST that turns recorded bytes into text.
 *
 * Every shape and route below was read off `core/src/voice.rs`, not inferred
 * from a name. Three facts are load-bearing and easy to get wrong by analogy
 * with the rest of the shell:
 *
 * **`armed` is a config fact, not a hardware one.** `armed = enabled &&
 * stt_command != ""` (`voice.rs:799-832`) — it says the shell is configured to
 * try, never that a GPU or a model is actually reachable on this machine.
 * `GET /voice/config` is always 200; there is no 503 to catch here.
 *
 * **`POST /voice/capture`'s 204 is a refusal wearing a success status.** It
 * means "nothing was heard" — a muted mic, the wrong input device — and
 * {@link apiFetch} already returns `undefined` for 204/205 without throwing.
 * {@link postCapture}'s return type says so at the call site: `undefined` is
 * not "still loading" here, it is the whole answer, and the caller must name
 * that outcome rather than read the absence as success.
 *
 * **Dictations have no delete.** `http.rs:293` calls them "read by hand for
 * prompt tuning, not by the shell" — they expire by retention
 * (`retain_dictations_days`) instead. Only {@link useDeleteMemo} exists; there
 * is deliberately no dictation equivalent.
 */

/** `GET /voice/config` — infallible 200, exactly as `VoiceConfigView` serialises. */
export interface VoiceConfigView {
  /** The whole availability signal for the capture Teach — see this module's header. */
  armed: boolean;
  hints: string[];
  /** The cleanup prompt's TEXT. There is no file path on the wire — it lives in `.ai/voice.yaml`. */
  cleanup_prompt: string;
  /** `null` means cleanup is unarmed: every capture comes back `"raw"`. */
  cleanup_model: string | null;
  retain_dictations_days: number;
  /** `""` when unconfigured — never `null`. */
  hotkey: string;
  /** `""` when unconfigured — never `null`. */
  memo_hotkey: string;
  max_capture_seconds: number;
  max_body_bytes: number;
}

/**
 * One memo or one dictation — identical row shape for both lists.
 *
 * `raw_text` is ALWAYS populated, the "as heard" text. `clean_text` is
 * non-null only when `cleanup_state === "cleaned"`; `ui/state-map.ts`'s
 * `voice_cleanup` domain is what keeps `"raw"` and `"shrunk"` from reading as
 * the same fact — both leave the raw transcript on screen, for different
 * reasons.
 */
export interface Capture {
  id: number;
  kind: "memo" | "dictation";
  created_at: string;
  duration_ms: number;
  raw_text: string;
  clean_text: string | null;
  cleanup_state: "cleaned" | "raw" | "shrunk";
  model: string | null;
}

/** What `POST /voice/capture` answers on 200 — `id: 0` means the DB write failed while the text is still valid. */
export interface CaptureResult {
  id: number;
  text: string;
  state: "cleaned" | "raw" | "shrunk";
}

/** The pillar's own configuration — read-only from this page. */
export function useVoiceConfig() {
  return useQuery({
    queryKey: keys.voice.config,
    queryFn: () => apiFetch<VoiceConfigView>("/voice/config"),
    refetchInterval: POLL.slow,
  });
}

/**
 * The memos, newest first — `GET /voice/memos`. No params, no pagination, the
 * full corpus (`ORDER BY created_at DESC, id DESC`).
 *
 * `POLL.voice`, not `POLL.queue`: captures land from outside the window — a
 * hotkey pressed while the shell is not the focused app — so this list has no
 * user gesture to refetch after, the way a mutation elsewhere in the shell
 * does.
 */
export function useMemos() {
  return useQuery({
    queryKey: keys.voice.memos,
    queryFn: () => apiFetch<Capture[]>("/voice/memos"),
    refetchInterval: POLL.voice,
    placeholderData: keepPreviousData,
  });
}

/** The dictations, newest first — `GET /voice/dictations`. Same shape and cadence as {@link useMemos}. */
export function useDictations() {
  return useQuery({
    queryKey: keys.voice.dictations,
    queryFn: () => apiFetch<Capture[]>("/voice/dictations"),
    refetchInterval: POLL.voice,
    placeholderData: keepPreviousData,
  });
}

/**
 * Delete one memo. `DELETE /voice/memos/{id}` — 204 on success.
 *
 * There is no dictation equivalent — see this module's header. Invalidates
 * only `keys.voice.memos`, not the whole `voice` namespace: a memo deletion
 * changes nothing about the config or the dictation list.
 */
export function useDeleteMemo() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (id: number) => apiFetch<void>(`/voice/memos/${id}`, { method: "DELETE" }),
    retry: false,
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: keys.voice.memos });
    },
  });
}

/**
 * Post one capture's raw bytes. `POST /voice/capture?kind=&duration_ms=&format=`
 * — verified against `voice.rs:634-720`.
 *
 * A direct {@link apiFetch} call, deliberately OUTSIDE react-query: this is
 * not a cache-backed read and not a mutation a list needs to invalidate
 * against by itself — the page invalidates {@link useMemos} /
 * {@link useDictations} once it knows which list the capture landed in.
 *
 * The body is RAW BINARY audio, not multipart and not base64 JSON, so the
 * caller must set the content type explicitly — {@link apiFetch}'s underlying
 * `request` only fills in `application/json` when no `Content-Type` header is
 * present at all, and a caller that let that default through would send audio
 * bytes mislabelled as JSON.
 *
 * `kind` and `durationMs` are required by the route; `format` defaults to
 * `"wav"`, which is what `lib/audio.ts`'s `encodeCapture` always produces.
 *
 * Returns `undefined` on `204` — "nothing was heard" — and does NOT throw for
 * it. See this module's header: the caller must read `undefined` as that
 * outcome, not as an unanswered request.
 */
export function postCapture(
  bytes: Uint8Array,
  kind: "dictation" | "memo",
  durationMs: number,
  format = "wav",
): Promise<CaptureResult | undefined> {
  const params = new URLSearchParams({
    kind,
    duration_ms: String(Math.max(0, Math.round(durationMs))),
    format,
  });
  return apiFetch<CaptureResult | undefined>(`/voice/capture?${params.toString()}`, {
    method: "POST",
    headers: { "Content-Type": "application/octet-stream" },
    body: bytes,
  });
}

/* ------------------------------------------------------- capture phases -- */

/**
 * The page's own capture state, independent of anything the microphone is
 * doing — `idle` (nothing in flight), `recording` (the webview is capturing
 * samples) or `transcribing` (recording has stopped and the bytes are being
 * encoded and posted).
 */
export type VoicePhase = "idle" | "recording" | "transcribing";

/**
 * Every signal that can move the phase, named after where it comes from
 * rather than after the phase it produces — so the reducer below is the one
 * place that decision is made.
 *
 * `"start"`/`"stop"` are the webview's own `voice://start` / `voice://stop`
 * event payloads — the real system hotkey, fired by the host whether or not
 * this window has focus. `"hotkey"` is `voice_hotkey`'s resolved value, from
 * the page's own capture buttons standing in for that same hotkey while the
 * window IS focused; `"busy"` there is not a phase the UI shows on its own,
 * it means the host was already mid-transcription and this press did nothing
 * — read as `transcribing`, the phase that was already true. `"capture-done"`
 * and `"capture-error"` are {@link postCapture} settling, one way or the
 * other; `"abandon"` is `voice_abandon` — a capture withdrawn without ever
 * being posted.
 */
export type VoiceEvent =
  | { type: "start"; kind: "dictation" | "memo" }
  | { type: "stop" }
  | { type: "hotkey"; phase: "recording" | "transcribing" | "busy" }
  | { type: "capture-done" }
  | { type: "capture-error" }
  | { type: "abandon" };

/**
 * The whole capture state machine, in one pure function — testable without a
 * microphone, a `MediaStream` or a `Tauri` runtime, and test-first by design:
 * the page's `idle | recording | transcribing` transitions are a fact about
 * this reducer before they are a fact about anything on screen.
 *
 * Takes the event alone rather than `(phase, event)` — every event here names
 * an unambiguous destination on its own; none of the six needs to know where
 * the machine already was to know where it is going next.
 */
export function phaseAfter(event: VoiceEvent): VoicePhase {
  switch (event.type) {
    case "start":
      return "recording";
    case "stop":
      return "transcribing";
    case "hotkey":
      return event.phase === "busy" ? "transcribing" : event.phase;
    case "capture-done":
    case "capture-error":
    case "abandon":
      return "idle";
  }
}
