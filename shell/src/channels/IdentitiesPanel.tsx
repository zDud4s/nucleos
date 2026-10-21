import { useState } from "react";
import { Link } from "@tanstack/react-router";
import { DAEMON_NAME } from "../copy";
import { isApiRefusal } from "../data/client";
import {
  useChannelIdentities,
  useRemoveIdentity,
  useSetHomeConversation,
  type Channel,
  type ChannelIdentity,
} from "../data/channels";
import { Badge } from "../ui/Badge";
import { Button } from "../ui/Button";
import { Callout } from "../ui/Callout";
import { ErrorNote } from "../ui/ErrorNote";
import { Modal } from "../ui/Modal";
import { Quiet } from "../ui/Quiet";
import { RefusalNote } from "../ui/RefusalNote";
import { Section } from "../ui/Section";
import { Skeleton } from "../ui/Skeleton";
import "./identities.css";

const SENTENCES: Record<string, string> = {
  unknown_identity: "that sender is no longer paired on this channel",
  malformed_ref: "that conversation reference is not valid",
  unknown_conversation: "that conversation is no longer listed for this sender",
};

type Conversation = ChannelIdentity["conversations"][number];

/** What the confirmation is about: a whole sender, or one of their conversations. */
interface Removal {
  sender: ChannelIdentity;
  conversation: Conversation | null;
}

/**
 * Who is paired on a channel and where (annex E.5, E.8).
 *
 * Grouped by sender, conversations in the order the daemon sends them. A
 * `group` conversation covers every topic in it, which is what "all topics"
 * says; the "home" badge marks the one conversation the channel writes to by
 * default. The channel's state is deliberately ignored: a `not_configured`
 * channel can still hold identities from an earlier configuration, and hiding
 * them would hide access that is still open.
 *
 * The panel also acts: "Make home" moves the home to one of a sender's
 * conversations, and the two removals sit behind a modal that says what the
 * removal leaves behind. Removing the last identity is allowed (E.5), so the
 * modal warns instead of refusing: "no home" when the home goes with it, "no
 * owner" when nobody would be left to talk to the bot. `primary` sharpens the
 * first warning, because a primary without a home silences the notifications.
 */
export function IdentitiesPanel({ channel, primary = false }: { channel: Channel; primary?: boolean }) {
  const identities = useChannelIdentities(channel.id);
  const setHome = useSetHomeConversation(channel.id);
  const remove = useRemoveIdentity(channel.id);
  const [removal, setRemoval] = useState<Removal | null>(null);
  const [outcome, setOutcome] = useState<string | null>(null);
  const busy = setHome.isPending || remove.isPending;

  if (identities.isPending) {
    return <Skeleton label="Reading the identities" lines={3} />;
  }
  if (identities.isError) {
    const error = identities.error;
    if (isApiRefusal(error)) return <RefusalNote refusal={error} />;
    return (
      <ErrorNote onRetry={() => void identities.refetch()}>
        Could not reach the {DAEMON_NAME}: the paired identities are not known
      </ErrorNote>
    );
  }

  const { identities: senders, projects } = identities.data;

  // Computed from the data already read: no extra request to know what a removal leaves behind.
  const warnings: string[] = [];
  if (removal !== null) {
    const { sender, conversation } = removal;
    const losesHome = conversation === null ? sender.conversations.some((c) => c.home) : conversation.home;
    if (losesHome) {
      warnings.push("this channel will have no home conversation");
      if (primary) warnings.push("notifications will stop until you pick a new home");
    }
    const losesSender = conversation === null || sender.conversations.length === 1;
    if (senders.length === 1 && losesSender) {
      warnings.push(
        "this channel will have no owner: nobody will be able to talk to the bot until someone is paired again",
      );
    }
  }

  const closeRemoval = () => {
    setRemoval(null);
    remove.reset();
  };
  const confirmRemoval = () => {
    if (removal === null) return;
    remove.mutate(
      {
        sender: removal.sender.sender_ref,
        ...(removal.conversation === null ? {} : { conversation: removal.conversation.conversation_ref }),
      },
      {
        onSuccess: (done) => {
          setOutcome(`removed ${done.removed}${done.home_cleared ? ", the home conversation was cleared" : ""}`);
          setRemoval(null);
        },
      },
    );
  };
  const who = removal === null ? "" : (removal.sender.sender_name ?? removal.sender.sender_ref);

  return (
    <div className="identities-panel">
      {outcome !== null && (
        <div className="identities-outcome">
          <Quiet compact announce says={outcome} />
        </div>
      )}
      {setHome.isError &&
        (isApiRefusal(setHome.error) ? (
          <RefusalNote refusal={setHome.error} sentences={SENTENCES} />
        ) : (
          <ErrorNote>Could not reach the {DAEMON_NAME}: the home was not changed</ErrorNote>
        ))}
      <Modal
        open={removal !== null}
        onOpenChange={(open) => {
          if (!open && !remove.isPending) closeRemoval();
        }}
        title={removal?.conversation ? "Remove conversation" : "Remove sender"}
        description={
          removal?.conversation
            ? `Conversation ${removal.conversation.conversation_ref} of ${who} will be unpaired.`
            : `${who} will no longer be able to talk to the bot.`
        }
        footer={
          <>
            <Button variant="ghost" onClick={closeRemoval} disabled={remove.isPending}>
              Cancel
            </Button>
            <Button variant="danger-solid" onClick={confirmRemoval} disabled={remove.isPending}>
              Remove
            </Button>
          </>
        }
      >
        {warnings.length > 0 && (
          <div className="identities-warnings">
            {warnings.map((w) => (
              <Callout key={w} tone="pending">
                {w}
              </Callout>
            ))}
          </div>
        )}
        {remove.isError &&
          (isApiRefusal(remove.error) ? (
            <RefusalNote refusal={remove.error} sentences={SENTENCES} />
          ) : (
            <ErrorNote>Could not reach the {DAEMON_NAME}: nothing was removed</ErrorNote>
          ))}
      </Modal>
      <Section label="Paired" level={3}>
        {senders.length === 0 ? (
          <Quiet compact says="nobody is paired on this channel yet" />
        ) : (
          senders.map((sender) => (
            <SenderGroup
              key={sender.sender_ref}
              sender={sender}
              busy={busy}
              onMakeHome={(conversation_ref) => {
                setOutcome(null);
                setHome.mutate({ sender: sender.sender_ref, conversation_ref });
              }}
              onRemove={(conversation) => {
                setOutcome(null);
                remove.reset();
                setRemoval({ sender, conversation });
              }}
            />
          ))
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

function SenderGroup({
  sender,
  busy,
  onMakeHome,
  onRemove,
}: {
  sender: ChannelIdentity;
  busy: boolean;
  onMakeHome: (conversationRef: string) => void;
  onRemove: (conversation: Conversation | null) => void;
}) {
  const named = sender.sender_name !== null;
  return (
    <div className="identities-sender">
      <div className="identities-sender-head">
        <span className="identities-sender-name">{named ? sender.sender_name : sender.sender_ref}</span>
        {named && <span className="identities-sender-ref">{sender.sender_ref}</span>}
        <span className="identities-actions">
          <Button variant="quiet" disabled={busy} onClick={() => onRemove(null)}>
            Remove sender
          </Button>
        </span>
      </div>
      <ul className="identities-conversations">
        {sender.conversations.map((c) => (
          <li
            key={c.conversation_ref}
            className="identities-conversation"
            data-testid="identity-conversation"
            data-ref={c.conversation_ref}
          >
            <span className="identities-conversation-kind">{c.kind}</span>
            <span className="identities-conversation-ref">{c.conversation_ref}</span>
            {c.home && <Badge tone="active">home</Badge>}
            {c.covers_threads && <Badge tone="info">all topics</Badge>}
            <span className="identities-actions">
              {!c.home && (
                <Button variant="quiet" disabled={busy} onClick={() => onMakeHome(c.conversation_ref)}>
                  Make home
                </Button>
              )}
              <Button variant="quiet" disabled={busy} onClick={() => onRemove(c)}>
                Remove conversation
              </Button>
            </span>
          </li>
        ))}
      </ul>
    </div>
  );
}
