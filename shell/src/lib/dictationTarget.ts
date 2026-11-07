/**
 * Where a dictation lands when the shell runs in a browser tab: the page's own focused field.
 *
 * On the desktop the host pastes into whatever application has focus. A browser tab has no such
 * reach, so the text is typed into the text field the person was in when the recording STARTED
 * (focus has usually moved by the time the transcript comes back), or goes to the clipboard.
 */

const TEXT_INPUT_TYPES = new Set(["", "text", "search", "url", "tel", "email", "number"]);
// "password" is left out on purpose: dictated text never enters a password box, it goes to the clipboard.

function isEditableHost(el: Element): el is HTMLElement {
  if (!(el instanceof HTMLElement)) return false;
  if (el.isContentEditable) return true;
  // jsdom does not implement `isContentEditable`; the attribute says the same thing.
  const attr = el.getAttribute("contenteditable");
  return attr === "" || attr === "true" || attr === "plaintext-only";
}

/** The text field that has focus right now, or `null` when focus is on a button, the body or nothing. */
export function focusedField(): HTMLElement | null {
  const el = document.activeElement;
  if (el === null) return null;
  if (el instanceof HTMLTextAreaElement) return el.disabled || el.readOnly ? null : el;
  if (el instanceof HTMLInputElement) {
    if (el.disabled || el.readOnly || !TEXT_INPUT_TYPES.has(el.type)) return null;
    return el;
  }
  return isEditableHost(el) ? el : null;
}

/**
 * Types `text` into `el` at its caret, replacing the selection. Returns whether it did.
 *
 * A React-controlled input ignores a plain `el.value = …`: React tracks the value through the
 * element's own setter, so the write goes through the PROTOTYPE's setter and an `input` event
 * follows, which is what makes `onChange` fire with the new value.
 */
export function insertIntoField(el: HTMLElement, text: string): boolean {
  if (!el.isConnected) return false;
  if (el instanceof HTMLInputElement || el instanceof HTMLTextAreaElement) {
    const proto = el instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
    const setter = Object.getOwnPropertyDescriptor(proto, "value")?.set;
    if (setter === undefined) return false;
    let start = el.value.length;
    let end = start;
    try {
      // `selectionStart` is null (or throws) on types such as email and number.
      if (el.selectionStart !== null) {
        start = el.selectionStart;
        end = el.selectionEnd ?? start;
      }
    } catch {
      // Keep the append-at-the-end default.
    }
    setter.call(el, el.value.slice(0, start) + text + el.value.slice(end));
    try {
      el.setSelectionRange(start + text.length, start + text.length);
    } catch {
      // Same types: no caret to move.
    }
    el.dispatchEvent(new Event("input", { bubbles: true }));
    return true;
  }
  if (isEditableHost(el)) {
    el.focus();
    const done = typeof document.execCommand === "function" && document.execCommand("insertText", false, text);
    if (!done) {
      el.textContent = (el.textContent ?? "") + text;
      el.dispatchEvent(new Event("input", { bubbles: true }));
    }
    return true;
  }
  return false;
}

const MODIFIERS: Record<string, "ctrl" | "alt" | "shift" | "meta"> = {
  ctrl: "ctrl",
  control: "ctrl",
  commandorcontrol: "ctrl",
  cmdorctrl: "ctrl",
  alt: "alt",
  option: "alt",
  shift: "shift",
  meta: "meta",
  cmd: "meta",
  command: "meta",
  super: "meta",
  win: "meta",
};

/**
 * The chord a browser tab can actually see. The OS keeps the Windows/Super key, so `Ctrl+Alt+Super+D`
 * is pressed as `Ctrl+Alt+D` in a page. The stored configuration is not touched.
 */
export function pageChord(chord: string): string {
  return chord
    .split("+")
    .filter((p) => !["super", "win"].includes(p.trim().toLowerCase()))
    .join("+");
}

/** Whether a key event is the chord written as `Ctrl+Alt+D` (the form `voice.yaml` uses). `""` never matches. */
export function chordMatches(event: KeyboardEvent, chord: string): boolean {
  if (chord.trim() === "") return false;
  const parts = chord.split("+").map((p) => p.trim().toLowerCase());
  const key = parts.pop();
  if (key === undefined || key === "") return false;
  const want = { ctrl: false, alt: false, shift: false, meta: false };
  for (const part of parts) {
    const mod = MODIFIERS[part];
    if (mod === undefined) return false;
    want[mod] = true;
  }
  // CommandOrControl is Ctrl in a browser on Windows/Linux and Meta on macOS; either satisfies it.
  const wantsCmdOrCtrl = parts.some((p) => p === "commandorcontrol" || p === "cmdorctrl");
  const ctrlOk = wantsCmdOrCtrl ? event.ctrlKey || event.metaKey : event.ctrlKey === want.ctrl;
  const metaOk = wantsCmdOrCtrl ? true : event.metaKey === want.meta;
  if (!ctrlOk || !metaOk || event.altKey !== want.alt || event.shiftKey !== want.shift) return false;
  const pressed = event.key.toLowerCase();
  const code = event.code.toLowerCase();
  return pressed === key || code === key || code === `key${key}` || code === `digit${key}`;
}
