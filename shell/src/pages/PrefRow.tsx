import type { ReactNode } from "react";
import { Row, Tip } from "../ui";

/**
 * One line of a machine-settings list: the label on the left with its ⓘ tip right beside it, the
 * control on the right at a shared width.
 *
 * It is a `Row`, so it must sit inside a `Rows`. What would have been a helper paragraph under the
 * control goes in `tip`, and appears only while the owner asks for it. `note` is for a result or an
 * error that belongs to this row: it sits under the line, full width, and only while there is one.
 */
export function PrefRow({
  label,
  htmlFor,
  tip,
  tipName,
  note,
  stacked = false,
  children,
}: {
  label: string;
  /** The id of the control the label names; omit when the control names itself (a hidden-label switch). */
  htmlFor?: string;
  tip?: ReactNode;
  tipName?: string;
  note?: ReactNode;
  /** The label takes the first line and the control the full width of a second, for a wide control. */
  stacked?: boolean;
  children: ReactNode;
}) {
  return (
    <Row dense>
      <div className={stacked ? "sy-prefrow sy-prefrow-stacked" : "sy-prefrow"}>
        <span className="sy-prefrow-label">
          {htmlFor === undefined ? <span>{label}</span> : <label htmlFor={htmlFor}>{label}</label>}
          {tip === undefined ? null : <Tip tip={tip} name={tipName ?? `About ${label.toLowerCase()}`} />}
        </span>
        <div className="sy-prefrow-control">{children}</div>
      </div>
      {note}
    </Row>
  );
}
