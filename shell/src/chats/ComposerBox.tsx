import type {
  ClipboardEvent,
  KeyboardEvent,
  ReactNode,
  RefObject,
} from "react";
import { ArrowUp } from "lucide-react";

/**
 * One object you type into, with the actions inside it — see `.chats-composer-box`.
 *
 * Controlled and router-free: the words, the keys and the sending all belong to whoever mounts it.
 * Enter submits and Shift+Enter breaks the line, because that is what every chat anybody has ever
 * used does — and a textarea does the opposite by default.
 *
 * `onKeyDown` runs first and returns true when it took the key (a list open under the caret owns
 * the arrows and the Enter for as long as it is showing); only then does the box's own Enter run.
 * The textarea carries `aria-label="Message"` and the send button `aria-label="Send"`.
 */
export function ComposerBox({
  text,
  onText,
  onSubmit,
  onKeyDown,
  onSelect,
  onPaste,
  boxRef,
  line,
  actions,
  sendDisabled,
  placeholder = "Say something…",
  sendLabel = "Send",
}: {
  text: string;
  /** The new text, and where the caret is in it. */
  onText: (text: string, caret: number) => void;
  onSubmit: () => void;
  /** Runs before the box's own handling; return true when it took the key. */
  onKeyDown?: (event: KeyboardEvent<HTMLTextAreaElement>) => boolean;
  /** The caret moved without the text changing. */
  onSelect?: (caret: number) => void;
  onPaste?: (event: ClipboardEvent<HTMLTextAreaElement>) => void;
  boxRef: RefObject<HTMLTextAreaElement | null>;
  /** Siblings after the textarea on its line (the voice toggle). */
  line?: ReactNode;
  /** The controls along the foot of the box, before the send button. */
  actions?: ReactNode;
  sendDisabled: boolean;
  placeholder?: string;
  sendLabel?: string;
}) {
  return (
    <div className="chats-composer-box">
      {/* The control beside the textarea shares its flex line rather than owning a row: the
          textarea takes what is left and `align-items: flex-start` keeps the control at the top as
          the text grows down past it. */}
      <div className="chats-composer-line">
        <textarea
          className="chats-composer-text"
          placeholder={placeholder}
          ref={boxRef}
          onPaste={onPaste}
          /* The floor, not the size. `field-sizing: content` grows the box from here; this is what
             it falls back to where that is unsupported. */
          rows={1}
          aria-label="Message"
          value={text}
          onChange={(event) =>
            onText(event.target.value, event.target.selectionStart)
          }
          onSelect={(event) => onSelect?.(event.currentTarget.selectionStart)}
          onKeyDown={(event) => {
            if (onKeyDown?.(event) === true) return;
            if (event.key !== "Enter" || event.shiftKey) return;
            event.preventDefault();
            onSubmit();
          }}
        />
        {line}
      </div>
      <div className="chats-composer-actions">
        {actions}
        <button
          type="button"
          className="chats-send"
          aria-label={sendLabel}
          disabled={sendDisabled}
          onClick={onSubmit}
        >
          <ArrowUp className="chats-send-icon" aria-hidden="true" />
        </button>
      </div>
    </div>
  );
}
