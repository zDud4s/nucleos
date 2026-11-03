import type { ReactNode } from "react";

/**
 * Kbd (#4): a keyboard-shortcut chip. `<Kbd>⌘K</Kbd>` or `<Kbd keys={["Ctrl", "K"]} />`.
 * Decorative text for a shortcut that is announced elsewhere, so it is not focusable.
 */
export function Kbd({ keys, children, className }: { keys?: string[]; children?: ReactNode; className?: string }) {
  const cls = `ui-kbd${className ? ` ${className}` : ""}`;
  if (keys !== undefined) {
    return (
      <span className="ui-kbd-group">
        {keys.map((k, i) => (
          <kbd key={`${k}-${i}`} className={cls}>
            {k}
          </kbd>
        ))}
      </span>
    );
  }
  return <kbd className={cls}>{children}</kbd>;
}
