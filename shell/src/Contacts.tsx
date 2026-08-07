import { useCallback, useEffect, useState } from "react";
import {
  decideContactMerge, getContactMerges, getContacts, setSenderVerdict, unmergeContact,
  type ConnectionState, type Correspondent, type MergeSide, type MergeSuggestion,
  type SenderVerdict,
} from "./api";
import { relativeTime } from "./derive";
import { Badge, Button, ConfirmButton, ErrorNote, Panel, Teach } from "./ui";

interface ContactsProps {
  token: string | null;
  connection: ConnectionState;
}

function verdictLabel(verdict: string | null): string | null {
  if (verdict === "pin") return "always urgent";
  if (verdict === "mute") return "always noise";
  return null;
}

function Side({ side }: { side: MergeSide }) {
  const label = verdictLabel(side.verdict);
  return (
    <div className="m-side">
      <b>{side.display_name ?? side.addresses[0]}</b>
      {/* Every address under this contact, because the question is about identity and an address
          the person does not recognise is the whole reason to answer no. */}
      <ul className="m-addrs">
        {side.addresses.map((address) => <li key={address}>{address}</li>)}
      </ul>
      <span className="m-count">
        {side.messages_in} message{side.messages_in === 1 ? "" : "s"} in
      </span>
      {label !== null && <Badge tone={side.verdict === "pin" ? "active" : "off"}>{label}</Badge>}
    </div>
  );
}

interface SuggestionsProps {
  token: string;
  suggestions: MergeSuggestion[];
  onDecided: () => void;
}

/**
 * Questions the núcleo has asked about who is who.
 *
 * The heuristic files a suggestion and a human answers — it never merges on its own, because a
 * wrong guess silently fuses two people's histories and you find out months later from the verdict
 * it caused. This is the answering surface; without it the suggestion was created by a sweep that
 * nothing ran, hidden by a list that filtered it out, and refused by both decision endpoints.
 */
function Suggestions({ token, suggestions, onDecided }: SuggestionsProps) {
  const [busy, setBusy] = useState<number | null>(null);
  const [failed, setFailed] = useState<{ id: number; text: string } | null>(null);

  async function decide(proposalId: number, accept: boolean) {
    setBusy(proposalId);
    setFailed(null);
    const result = await decideContactMerge(token, proposalId, accept);
    setBusy(null);
    if (!result.ok) {
      setFailed({
        id: proposalId,
        // The daemon refuses to join two people you told it opposite things about, rather than
        // picking a winner and discarding one of your instructions. Naming the way out is the
        // whole value of separating this from a generic failure.
        text: result.status === 409
          ? "These two carry opposite standing decisions. Withdraw one of them below, then answer again."
          : result.status === 404
            ? "That suggestion is gone."
            : "The daemon did not take that answer.",
      });
      return;
    }
    onDecided();
  }

  return (
    <Panel title="Same person?" aside={`${suggestions.length} to answer`}>
      {suggestions.map((one) => (
        <article className="feed-item merge-ask" key={one.proposal_id}>
          <div className="m-pair">
            <Side side={one.keep} />
            <span className="m-join">=</span>
            <Side side={one.absorb} />
          </div>
          <p className="a-note">{one.reasoning}</p>
          <div className="a-actions">
            {/* Confirmed on the way in, not on the way out: joining is undoable by splitting them
                again, but it is still a statement about two real people. */}
            <ConfirmButton
              size="sm"
              variant="approve"
              confirmLabel="Confirm same person?"
              disabled={busy !== null}
              onConfirm={() => void decide(one.proposal_id, true)}
            >
              Same person
            </ConfirmButton>
            <Button
              size="sm"
              disabled={busy !== null}
              title="The núcleo remembers this answer and stops suggesting this pair."
              onClick={() => void decide(one.proposal_id, false)}
            >
              Different people
            </Button>
          </div>
          {failed !== null && failed.id === one.proposal_id && (
            <ErrorNote>{failed.text}</ErrorNote>
          )}
        </article>
      ))}
    </Panel>
  );
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
function Roster({ token }: { token: string }) {
  const [contacts, setContacts] = useState<Correspondent[] | null>(null);
  const [suggestions, setSuggestions] = useState<MergeSuggestion[]>([]);
  const [loading, setLoading] = useState(true);
  const [filter, setFilter] = useState("");
  const [deciding, setDeciding] = useState<string | null>(null);
  const [failed, setFailed] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    const [next, asked] = await Promise.all([
      getContacts(token),
      getContactMerges(token),
    ]);
    setContacts(next);
    setSuggestions(asked ?? []);
    setLoading(false);
  }, [token]);

  /**
   * Splits an address back out into a person of its own.
   *
   * Offered only where a human actually joined something (`linked_by === "human"`), because that is
   * the only case with anything to undo — and it is what makes approving a merge a safe thing to
   * do. The daemon restores exactly what was there, counters included, since the join never moved
   * them.
   */
  async function split(address: string) {
    setDeciding(address);
    setFailed(null);
    const result = await unmergeContact(token, address);
    setDeciding(null);
    if (!result.ok) {
      setFailed("Could not split that address out.");
      return;
    }
    await refresh();
  }

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

  // How many addresses share each contact, so a merged pair can be told apart from a lone address.
  const sharing = new Map<number, number>();
  for (const one of contacts ?? []) {
    sharing.set(one.contact_id, (sharing.get(one.contact_id) ?? 0) + 1);
  }

  return (
    <>
      <h1 className="headline">
        {/* Silent until the roster is in hand: "nobody yet" is a real answer about a real mailbox,
            and printing it while the request is still out states it about one nobody has read. */}
        {contacts === null
          ? <>Who the núcleo knows.</>
          : contacts.length === 0
            ? <>Nobody yet.</>
            : <><em>{contacts.length}</em> {contacts.length === 1 ? "address" : "addresses"} write to you.</>}
      </h1>
      <div className="statusline">
        <span>{decided} with a standing decision</span>
        {/* Said plainly because it is the only channel wired in: a name here means a mail address,
            and a person you only ever hear from elsewhere is not yet in this list. */}
        <span>everyone here arrived through <b>mail</b></span>
        {suggestions.length > 0 && (
          <span>{suggestions.length} identity question{suggestions.length === 1 ? "" : "s"} waiting</span>
        )}
      </div>
      {suggestions.length > 0 && (
        <Suggestions
          token={token}
          suggestions={suggestions}
          onDecided={() => void refresh()}
        />
      )}
    <Panel
      title="People"
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
            {/* Only where a human joined something AND something is still joined: the flag alone
                would keep claiming a merge after the other half was split back out. */}
            {one.linked_by === "human" && (sharing.get(one.contact_id) ?? 1) > 1 && (
              <Badge tone="pending">merged</Badge>
            )}
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
            {one.linked_by === "human" && (sharing.get(one.contact_id) ?? 1) > 1 && (
              <ConfirmButton
                size="sm"
                confirmLabel="Confirm split?"
                disabled={deciding !== null}
                onConfirm={() => void split(one.address)}
              >
                Not the same person
              </ConfirmButton>
            )}
          </div>
        </article>
      ))}
      {failed !== null && <ErrorNote>{failed}</ErrorNote>}
    </Panel>
    </>
  );
}

/**
 * Who the núcleo knows, and what has been decided about them.
 *
 * Its own tab rather than a view inside Mail, because a contact is not a mail object. Mail is
 * simply the only channel wired in today; Slack and whatever follows will pour into this same
 * list, and a roster that lived under the mailbox would mean deciding twice who someone is — once
 * per channel — with two places to look for the pin that explains a verdict.
 *
 * The daemon's side is already built that way: a contact owns its addresses, and the address is
 * what a channel contributes.
 */
function Contacts({ token, connection }: ContactsProps) {
  if (token === null || connection !== "connected") {
    return (
      <section className="contacts">
        <Teach title="Contacts are waiting for the daemon.">
          The roster lives in the núcleo, not here. Connect to it and everyone it knows — with the
          standing decisions you have made about them — comes back exactly as you left it.
        </Teach>
      </section>
    );
  }

  return (
    <section className="contacts">
      <Roster token={token} />
    </section>
  );
}

export default Contacts;
