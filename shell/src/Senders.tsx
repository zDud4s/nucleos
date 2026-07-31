import { useCallback, useEffect, useState } from "react";
import {
  getContacts, setSenderVerdict,
  type Correspondent, type SenderVerdict,
} from "./api";
import { relativeTime } from "./derive";
import { Badge, Button, ErrorNote, Panel, Teach } from "./ui";

interface SendersProps {
  token: string;
}

/**
 * Everyone who writes to you, and what has been decided about them.
 *
 * `contact_addresses` has been written on every inbound message since the table existed, and read
 * only by `priority.rs` to tell a stranger from someone you correspond with. Nothing showed it —
 * `contacts.rs` still carries `#[allow(dead_code)]` on three fields "consumed by the later contact
 * display surface". This is that surface for the part of it that is finished.
 *
 * It is also where a standing decision can be found again. Pinning happens on a message, and once
 * that message scrolls out of the queue the only trace of the pin is its effect — so a list of who
 * is pinned and who is muted is the difference between a rule and a rule you can audit.
 *
 * One row per ADDRESS rather than per person, deliberately: the daemon can propose merging two
 * addresses into one contact, but nothing in it performs the merge, so a merged view would report a
 * judgement nobody has made.
 */
function Senders({ token }: SendersProps) {
  const [contacts, setContacts] = useState<Correspondent[] | null>(null);
  const [loading, setLoading] = useState(true);
  const [filter, setFilter] = useState("");
  const [deciding, setDeciding] = useState<string | null>(null);
  const [failed, setFailed] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    const next = await getContacts(token);
    setContacts(next);
    setLoading(false);
  }, [token]);

  useEffect(() => { void refresh(); }, [refresh]);

  async function decide(address: string, next: SenderVerdict | null) {
    setDeciding(address);
    setFailed(null);
    const result = await setSenderVerdict(token, address, next);
    setDeciding(null);
    if (!result.ok) {
      setFailed("Could not record that decision.");
      return;
    }
    await refresh();
  }

  const needle = filter.trim().toLowerCase();
  const shown = (contacts ?? []).filter((one) =>
    needle === ""
      || one.address.includes(needle)
      || (one.display_name ?? "").toLowerCase().includes(needle));
  const decided = (contacts ?? []).filter((one) => one.verdict !== null).length;

  return (
    <Panel
      title="Senders"
      aside={contacts === null ? undefined : `${contacts.length} known · ${decided} decided`}
    >
      {loading && contacts === null && <p className="a-note">Loading…</p>}
      {!loading && contacts === null && (
        <ErrorNote>Could not read the correspondents from the daemon.</ErrorNote>
      )}
      {contacts !== null && contacts.length === 0 && (
        <Teach title="Nobody has written yet.">
          A correspondent appears here the first time a message arrives from them. The núcleo counts
          what comes in and whether you have ever written back, which is how it tells a stranger from
          someone you know.
        </Teach>
      )}
      {contacts !== null && contacts.length > 0 && (
        <div className="filters">
          <label className="wide">
            Find
            <input
              value={filter}
              placeholder="an address or a name"
              onChange={(event) => setFilter(event.target.value)}
            />
          </label>
        </div>
      )}
      {/* Filtered locally: the roster is a couple of hundred rows at most and already in hand, so
          asking the daemon again per keystroke would buy latency and nothing else. */}
      {contacts !== null && contacts.length > 0 && shown.length === 0 && (
        <p className="a-note">Nobody matches that.</p>
      )}
      {shown.map((one) => (
        <article className="feed-item" key={one.address}>
          <div className="f-meta">
            <span className="d-id">{one.display_name ?? one.address}</span>
            {one.verdict === "pin" && <Badge tone="active">always urgent</Badge>}
            {one.verdict === "mute" && <Badge tone="off">always noise</Badge>}
            {/* Someone you have written to is not a stranger, and the classifier treats them
                differently — a first-contact "urgent" is demoted, theirs is not. */}
            {one.outbound_ever === 1 && <Badge tone="shadow">you write back</Badge>}
          </div>
          <p className="f-body">
            {one.display_name !== null && <span className="s-addr">{one.address} — </span>}
            <b>{one.messages_in}</b> message{one.messages_in === 1 ? "" : "s"} in ·
            last {relativeTime(one.last_seen)} · known since {relativeTime(one.first_seen)}
          </p>
          <div className="a-actions">
            <span className="sender-standing">
              <Button
                size="sm"
                variant={one.verdict === "pin" ? "approve" : undefined}
                disabled={deciding === one.address}
                aria-pressed={one.verdict === "pin"}
                onClick={() => void decide(one.address, one.verdict === "pin" ? null : "pin")}
              >
                Always urgent
              </Button>
              <Button
                size="sm"
                variant={one.verdict === "mute" ? "approve" : undefined}
                disabled={deciding === one.address}
                aria-pressed={one.verdict === "mute"}
                onClick={() => void decide(one.address, one.verdict === "mute" ? null : "mute")}
              >
                Always noise
              </Button>
            </span>
          </div>
        </article>
      ))}
      {failed !== null && <ErrorNote>{failed}</ErrorNote>}
    </Panel>
  );
}

export default Senders;
