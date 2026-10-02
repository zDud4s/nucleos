import type { ReactNode } from "react";
import { XIcon } from "lucide-react";
import { Dialog as DialogPrimitive } from "radix-ui";

/** `sm` for a question and its two answers; `md` for a short form. Anything bigger is a page. */
export type ModalSize = "sm" | "md";

export interface ModalProps {
  /** Controlled: the page holds whether it is open, so a mutation can close it when it lands. */
  open: boolean;
  /** Called with `false` on Escape, a click on the scrim, and the close button. */
  onOpenChange: (open: boolean) => void;
  /**
   * The heading, and the dialog's accessible name. Required, because a dialog with no name is
   * announced as "dialog" and nothing else — the reader is dropped into a box without being told
   * what it is for.
   */
  title: string;
  /** One or two sentences under the title, read out with it when the dialog opens. */
  description?: ReactNode;
  /** The answers — `Button`s, the primary one last — on the bottom edge, right-aligned. */
  footer?: ReactNode;
  size?: ModalSize;
  children?: ReactNode;
}

/**
 * A small window over the page, with the page dimmed and blurred behind it.
 *
 * For a decision that has to be made before anything else on the page is touched — a confirmation
 * worth more than `ConfirmButton`'s second click, or a short form that does not deserve a route.
 * Everything else belongs on the page: a modal takes the page away from the reader, and one used
 * for information they might want to compare against the page is the wrong tool.
 *
 * Built on Radix's dialog, as `ui/vendor/dialog.tsx` is, for what Radix gets right and a hand-rolled
 * one gets wrong: focus moves in and is trapped, returns to the trigger on close, Escape closes it,
 * the page under it is `inert` to a screen reader, and the body stops scrolling. What it does NOT
 * take from the vendor file is the dress: that one is shadcn's Tailwind, and this one wears the
 * `Panel`'s surface, border, radius and heading, so a modal reads as a panel lifted off the page
 * rather than as a component from another app.
 *
 * Animated with keyframes in `ui.css` on Radix's `data-state`, never a `<style>` element (the
 * production CSP refuses one), and not at all under reduced motion.
 */
export function Modal({ open, onOpenChange, title, description, footer, size = "sm", children }: ModalProps) {
  return (
    <DialogPrimitive.Root open={open} onOpenChange={onOpenChange}>
      <DialogPrimitive.Portal>
        <DialogPrimitive.Overlay className="ui-modal-scrim" />
        <DialogPrimitive.Content
          className={`ui-modal ui-modal-${size}`}
          // Radix warns when there is no Description; omitting it is a legitimate choice here.
          {...(description === undefined ? { "aria-describedby": undefined } : {})}
        >
          <div className="ui-modal-head">
            <DialogPrimitive.Title className="ui-modal-title">{title}</DialogPrimitive.Title>
            <DialogPrimitive.Close className="ui-icon-button ui-modal-close" aria-label="Close" title="Close">
              <XIcon className="ui-icon-button-glyph" strokeWidth={1.5} aria-hidden="true" />
            </DialogPrimitive.Close>
          </div>
          {description !== undefined && (
            <DialogPrimitive.Description className="ui-modal-description">{description}</DialogPrimitive.Description>
          )}
          {children !== undefined && <div className="ui-modal-body">{children}</div>}
          {footer !== undefined && <div className="ui-modal-foot">{footer}</div>}
        </DialogPrimitive.Content>
      </DialogPrimitive.Portal>
    </DialogPrimitive.Root>
  );
}
