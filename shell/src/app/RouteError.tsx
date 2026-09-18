import { Link } from "@tanstack/react-router";
import { CopyButton, ErrorNote, PageHeader } from "../ui";

export interface RouteErrorProps {
  /**
   * Whatever the page threw.
   *
   * `unknown` and not `Error`, because a `throw` is not obliged to hand over an
   * `Error` — a rejected promise carries whatever it was rejected with, and a
   * library that throws a string would otherwise crash the thing drawn to
   * survive a crash. Typed structurally rather than imported from the router so
   * this component can be rendered on its own, which is what its test does.
   */
  error: unknown;
  /**
   * The router's offer to re-render the boundary's subtree.
   *
   * Declared because the router passes it and the type should say so, and
   * deliberately not used. Re-rendering the same page against the same daemon
   * answer throws the same error, so a "Try again" here would be a control that
   * does nothing visible — which is worse than no control. The two ways out are
   * the ones below: leave, or take the report to somebody who can fix it.
   */
  reset?: () => void;
}

/**
 * A route that threw, drawn inside the shell rather than instead of it.
 *
 * The shape this replaces is a blank white rectangle: React unmounts the whole
 * tree under an uncaught throw, so before this existed a page that failed took
 * the rail, the connection line and the kill switch with it, and the window
 * became a thing with no way out and nothing to say. That is the worst possible
 * moment to remove the kill switch from somebody's reach.
 *
 * It is mounted per-route rather than on the root, so the boundary sits inside
 * `AppShell`'s `<Outlet />` and everything around the outlet survives. The root
 * is deliberately left without one — see `router.tsx`.
 *
 * The stack is expanded, never behind a press. This is not a production surface
 * with a support desk behind it: the person reading it is the person who will
 * fix it, or the person who will paste it to whoever does, and a disclosure
 * triangle between them and the only useful text on the screen buys nothing.
 */
export function RouteError({ error }: RouteErrorProps) {
  const message = error instanceof Error ? error.message : String(error);
  // Only a real `Error` carries one, and V8 already prefixes the stack with the
  // message — the report below repeats it anyway, because the message is what
  // the person read and the stack is what the machine wrote, and joining them
  // is cheaper than reasoning about which engine formatted which.
  const stack =
    error instanceof Error && typeof error.stack === "string" ? error.stack : "";
  const report = stack === "" ? message : `${message}\n${stack}`;

  // The same feature detection `ui/CopyOnce.tsx` does, and for the same two
  // reasons: jsdom has no `navigator.clipboard`, and a locked-down webview can
  // be missing one it looks like it has.
  const canCopy =
    typeof navigator !== "undefined" &&
    navigator.clipboard !== undefined &&
    typeof navigator.clipboard.writeText === "function";

  return (
    <div className="app-route-error">
      <PageHeader
        title="This page could not be drawn"
        headline="Everything around this page is still working — the rail, the connection line and the kill switch are where you left them. What follows is what the page itself threw."
      />
      <ErrorNote>{message}</ErrorNote>
      {stack === "" ? null : <pre className="app-route-error-stack">{stack}</pre>}
      <div className="app-route-error-actions">
        <Link to="/" className="ui-button ui-button-link">
          Back to Home
        </Link>
        {canCopy ? (
          <CopyButton value={report} label="this report" />
        ) : (
          /* Not an error state — the report above is selectable text either way.
             `CopyOnce` swaps its button for this same sentence for this same
             reason, and the class is the one that already styles it. */
          <p className="ui-copy-once-fallback">
            Select the report above and copy it — this window cannot copy for you.
          </p>
        )}
      </div>
    </div>
  );
}
