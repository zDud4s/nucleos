import { Link } from "@tanstack/react-router";
import { DAEMON_NAME } from "../copy";
import { isApiRefusal } from "../data/client";
import { useChannelIdentities, type Channel, type ChannelIdentity } from "../data/channels";
import { Badge } from "../ui/Badge";
import { ErrorNote } from "../ui/ErrorNote";
import { RefusalNote } from "../ui/RefusalNote";
import { Section } from "../ui/Section";
import "./identities.css";

/**
 * Who is paired on a channel and where (annex E.5, E.8), read-only.
 *
 * Grouped by sender, conversations in the order the daemon sends them. A
 * `group` conversation covers every topic in it, which is what "all topics"
 * says; the "home" badge marks the one conversation the channel writes to by
 * default. The channel's state is deliberately ignored: a `not_configured`
 * channel can still hold identities from an earlier configuration, and hiding
 * them would hide access that is still open.
 */
export function IdentitiesPanel({ channel }: { channel: Channel }) {
  const identities = useChannelIdentities(channel.id);

  if (identities.isPending) {
    return <p role="status">reading the identities…</p>;
  }
  if (identities.isError) {
    const error = identities.error;
    if (isApiRefusal(error)) return <RefusalNote refusal={error} />;
    return (
      <ErrorNote onRetry={() => void identities.refetch()}>
        Could not reach the {DAEMON_NAME} — the paired identities are not known
      </ErrorNote>
    );
  }

  const { identities: senders, projects } = identities.data;

  return (
    <div className="identities-panel">
      <Section label="Paired" level={3}>
        {senders.length === 0 ? (
          <p className="identities-empty">nobody is paired on this channel yet</p>
        ) : (
          senders.map((sender) => <SenderGroup key={sender.sender_ref} sender={sender} />)
        )}
      </Section>
      {projects.length > 0 && (
        <Section label="Projects" level={3}>
          <ul className="identities-projects">
            {projects.map((p) => (
              <li key={p.project_id} className="identities-project">
                <span>{p.project_id}</span>
                <span className="identities-conversation-ref">{p.conversation_ref}</span>
              </li>
            ))}
          </ul>
          {/* Read-only: the mapping lives in channels/<id>.yaml, edited in Machine. */}
          <Link to="/system/$view" params={{ view: "settings" }}>
            {`edit in Machine › channels/${channel.id}`}
          </Link>
        </Section>
      )}
    </div>
  );
}

function SenderGroup({ sender }: { sender: ChannelIdentity }) {
  const named = sender.sender_name !== null;
  return (
    <div className="identities-sender">
      <div className="identities-sender-head">
        <span className="identities-sender-name">{named ? sender.sender_name : sender.sender_ref}</span>
        {named && <span className="identities-sender-ref">{sender.sender_ref}</span>}
      </div>
      <ul className="identities-conversations">
        {sender.conversations.map((c) => (
          <li
            key={c.conversation_ref}
            className="identities-conversation"
            data-testid="identity-conversation"
            data-ref={c.conversation_ref}
          >
            <span>{c.kind}</span>
            <span className="identities-conversation-ref">{c.conversation_ref}</span>
            {c.home && <Badge tone="active">home</Badge>}
            {c.covers_threads && <Badge tone="info">all topics</Badge>}
          </li>
        ))}
      </ul>
    </div>
  );
}
