import { useState } from "react";
import type { ApiResult } from "../api";
import { Button, ErrorNote } from "../ui";

/**
 * What a refusal actually means, in the daemon's terms.
 *
 * Three distinct facts, and each one points somewhere different. A 409 is this conversation still
 * thinking and clears itself; a 503 is a conversation set to a model this machine does not have,
 * which only changes if you change it; anything else is the daemon not taking the message at all.
 * Collapsing them into one sentence would leave two of the three with nothing to do about it.
 */
export function refusalMessage(status: number): string {
  if (status === 409) return "This chat is still working on the previous message. Wait for it to land.";
  if (status === 503) {
    return "This conversation is set to the local model, and this machine has none configured. Switch it to the cloud, or configure one.";
  }
  return "The daemon did not take the message.";
}

interface ComposerProps {
  /** Whether a turn is in flight for THIS conversation. The daemon holds one turn slot per chat. */
  busy: boolean;
  onSend: (text: string) => Promise<ApiResult<number>>;
}

/** The box a message is written in, and the one place a refusal is explained. */
function Composer({ busy, onSend }: ComposerProps) {
  const [text, setText] = useState("");
  const [sending, setSending] = useState(false);
  const [failed, setFailed] = useState<string | null>(null);

  const asked = text.trim();
  const blocked = asked === "" || sending || busy;

  async function submit() {
    setSending(true);
    setFailed(null);
    const result = await onSend(asked);
    setSending(false);
    if (!result.ok) {
      // The text stays. Clearing it on a refusal loses the message and leaves nothing to retry.
      setFailed(refusalMessage(result.status));
      return;
    }
    setText("");
  }

  return (
    <>
      <form
        className="composer"
        onSubmit={(event) => {
          event.preventDefault();
          if (blocked) return;
          void submit();
        }}
      >
        <textarea
          rows={3}
          value={text}
          disabled={busy}
          placeholder={busy ? "Waiting for the current turn…" : "Ask the núcleo…"}
          onChange={(event) => setText(event.target.value)}
        />
        <Button type="submit" variant="approve" disabled={blocked}>
          {sending ? "Sending…" : busy ? "Working…" : "Send"}
        </Button>
      </form>
      {failed !== null && <ErrorNote>{failed}</ErrorNote>}
    </>
  );
}

export default Composer;
