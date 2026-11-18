import type { ChannelCapabilities } from "../data/channels";
import { Quiet, Row, Rows } from "../ui";
import { ChannelCardBox } from "./ChannelDetail";

type MediaKind = "image" | "file" | "audio";
const MEDIA_OUT: MediaKind[] = ["image", "file", "audio"];

// These sentences paraphrase §6.5 for presentation only. The authoritative
// wording lives in the núcleo's `strings.rs`; what the bot actually says to the
// owner may differ from what is shown here.
const FALLBACK = {
  buttons: 'reply "approve 12" / "reject 12"',
  threads: "per-project routing falls back to the home conversation with a [project] prefix",
  reply_to: "captures are answered with /cap <N> <text>",
  typing: "not used",
  edit: "not used",
  media_out: "that part is replaced by text: file available in NucleOS: <name>",
} as const;

interface Missing { key: string; label: string; text: string }

function missingOf(c: ChannelCapabilities): Missing[] {
  const out: Missing[] = [];
  if (!c.buttons) out.push({ key: "buttons", label: "buttons", text: FALLBACK.buttons });
  if (!c.threads) out.push({ key: "threads", label: "threads", text: FALLBACK.threads });
  if (!c.reply_to) out.push({ key: "reply_to", label: "reply to", text: FALLBACK.reply_to });
  if (!c.typing) out.push({ key: "typing", label: "typing indicator", text: FALLBACK.typing });
  if (!c.edit) out.push({ key: "edit", label: "edit", text: FALLBACK.edit });
  for (const kind of MEDIA_OUT) {
    if (!c.media_out.includes(kind)) {
      out.push({ key: `media_out_${kind}`, label: `sending ${kind}`, text: FALLBACK.media_out });
    }
  }
  return out;
}

/** The capabilities a channel lacks and what the núcleo does instead of each; nothing when none is lacking. */
export function CapabilityList({ capabilities }: { capabilities: ChannelCapabilities | null }) {
  if (capabilities === null) {
    return (
      <ChannelCardBox title="Capabilities" wide>
        <Quiet
          compact
          says="Capabilities are not known yet: the adapter has not said hello since the daemon started."
        />
      </ChannelCardBox>
    );
  }
  const missing = missingOf(capabilities);
  if (missing.length === 0) return null;
  return (
    <ChannelCardBox title="Capability gaps" wide>
      <Rows label="Lacking capabilities">
        {missing.map((m) => (
          <Row key={m.key} layout="line" dense className="ch-cap">
            <strong>{m.label}</strong>
            <span>{m.text}</span>
          </Row>
        ))}
      </Rows>
    </ChannelCardBox>
  );
}
