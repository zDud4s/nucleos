import { isApiRefusal } from "../data/client";
import { useSetPrimaryChannel, type Channel } from "../data/channels";
import { Button } from "../ui/Button";
import { ConfirmButton } from "../ui/ConfirmButton";
import { ErrorNote } from "../ui/ErrorNote";
import { Quiet } from "../ui/Quiet";
import { RefusalNote } from "../ui/RefusalNote";
import { Tip } from "../ui/Tip";

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
 * The "primary" marker itself is drawn by the channel list, not here. Clearing is behind a `ConfirmButton` because it leaves the notifications
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
  const reseeded = setPrimary.data?.reseeded === true;
  const error = setPrimary.error;

  return (
    <div className="ch-primary">
      {isPrimary ? (
        <ConfirmButton
          label="Clear primary"
          confirmLabel="Clear: notifications will have no destination"
          variant="ghost"
          disabled={setPrimary.isPending}
          onConfirm={() => setPrimary.mutate(null)}
        />
      ) : (
        <Button variant="ghost" disabled={setPrimary.isPending} onClick={() => setPrimary.mutate(channel.id)}>
          Make primary
        </Button>
      )}
      {channel.state === "offline" ? (
        <Tip name="About an offline channel" persist tip="the outbox keeps the notifications and delivers them when the adapter is back" />
      ) : null}
      {/* The answers take a line of their own under the buttons, wherever the control sits. */}
      {setPrimary.isSuccess && reseeded ? (
        <div className="ch-primary-note">
          <Quiet compact announce says="New channel">
            <Tip name="About a new channel" persist tip="new channel: it starts from now, without replaying what it missed" />
          </Quiet>
        </div>
      ) : null}
      {error ? (
        <div className="ch-primary-note">
          {isApiRefusal(error) ? (
            <RefusalNote refusal={error} sentences={SENTENCES} />
          ) : (
            <ErrorNote>could not change the primary channel</ErrorNote>
          )}
        </div>
      ) : null}
    </div>
  );
}
