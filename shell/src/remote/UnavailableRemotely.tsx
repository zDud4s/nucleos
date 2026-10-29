import { Callout } from "../ui/Callout";

/** The explicit inert state for something that only exists on the host machine. */
export function UnavailableRemotely({ feature }: { feature: string }) {
  return (
    <Callout tone="info" role="status">
      {feature} is only available on the host machine
    </Callout>
  );
}
