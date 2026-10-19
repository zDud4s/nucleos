import { isApiRefusal } from "../data/client";
import { useSetPrimaryChannel, type Channel } from "../data/channels";
import { Button } from "../ui/Button";
import { ConfirmButton } from "../ui/ConfirmButton";
import { ErrorNote } from "../ui/ErrorNote";
import { RefusalNote } from "../ui/RefusalNote";

/**
 * What the two refusals of `PUT /channels/primary` mean on this screen.
 *
 * Only the page knows that a 409 here is about a missing home conversation or
 * missing secrets; the shared floor would say "something about this has
 * already changed", which is true and useless.
 */
const SENTENCES: Record<string, string> = {
  no_home_conversation: "pair a conversation on this channel first: notifications would have nowhere to go",
  channel_not_configured: "this channel is missing its secrets: add them in Machine settings",
};

/**
 * Make a channel the primary one, or clear the choice.
 *
 * Clearing is behind a `ConfirmButton` because it leaves the notifications
 * without a destination. Choosing is a plain button: it is reversible with one
 * more press. An `offline` channel may still be chosen: the outbox keeps the
 * notifications and delivers them once the adapter is back, so refusing here
 * would punish a transient state.
 *
 * `reseeded` in the answer means the channel had never been primary: it starts
 * from now and does not replay what it missed. The hook types the answer as
 * `void`, so it is read through a narrow cast.
 */
export function PrimaryChannelControl({ channel, primary }: { channel: Channel; primary: string | null }) {
  const setPrimary = useSetPrimaryChannel();
  const isPrimary = primary === channel.id;
  const reseeded = (setPrimary.data as { reseeded?: boolean } | undefined)?.reseeded === true;
  const error = setPrimary.error;

  return (
    <div className="ch-primary">
      {isPrimary ? (
        <>
          <span className="ch-primary-badge">primary</span>
          <ConfirmButton
            label="Clear primary"
            confirmLabel="Clear: notifications will have no destination"
            variant="quiet"
            disabled={setPrimary.isPending}
            onConfirm={() => setPrimary.mutate(null)}
          />
        </>
      ) : (
        <Button variant="approve" disabled={setPrimary.isPending} onClick={() => setPrimary.mutate(channel.id)}>
          Make primary
        </Button>
      )}
      {channel.state === "offline" ? (
        <p className="ui-quiet">the outbox keeps the notifications and delivers them when the adapter is back</p>
      ) : null}
      {setPrimary.isSuccess && reseeded ? (
        <p className="ui-quiet" role="status">
          new channel: it starts from now, without replaying what it missed
        </p>
      ) : null}
      {error ? (
        isApiRefusal(error) ? (
          <RefusalNote refusal={error} sentences={SENTENCES} />
        ) : (
          <ErrorNote>could not change the primary channel</ErrorNote>
        )
      ) : null}
    </div>
  );
}
