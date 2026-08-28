// §spec novo-frontend
import { PageHeader, Teach } from "../ui";
import { sliceOf, type NavItem } from "../app/nav";

export interface PlaceholderProps {
  item: NavItem;
}

/**
 * A route that exists before its page does.
 *
 * Every item in §3.1 is navigable from the first day, and the ones that have no
 * page yet say so and name the slice that brings them. The alternative — a
 * sidebar that grows an item at a time — hides the shape of the app from the
 * person using it and makes each slice feel like a surprise instead of an
 * arrival.
 *
 * This is a {@link Teach} rather than an error or a spinner. Nothing is wrong
 * here: the route works, the shell is intact, the page is simply not built. A
 * "not found" would be a lie about a path the app itself put in the sidebar.
 *
 * An item the *núcleo* cannot serve at all — Teams, whose routes do not exist
 * in `http.rs` — says that separately and in the headline, because it is a
 * different fact with a different fix: nobody is going to build that page until
 * the daemon grows the routes under it.
 */
export function Placeholder({ item }: PlaceholderProps) {
  return (
    <>
      <PageHeader
        title={item.label}
        headline={item.disabled === undefined ? undefined : `not yet wired — ${item.disabled}`}
      />
      <Teach title={`${item.label} is not built yet`}>
        <p>
          The route is real and the shell around it is the shell you will keep — only this page is
          missing. It arrives with the {sliceOf(item)} slice.
        </p>
        {item.disabled === undefined ? null : (
          <p>
            Until then there is nothing here to switch on: {item.disabled}, so controls on this page would
            have nothing to call.
          </p>
        )}
      </Teach>
    </>
  );
}
