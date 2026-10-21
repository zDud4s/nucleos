import type { ReactNode, TdHTMLAttributes, ThHTMLAttributes } from "react";

export interface TableProps {
  /** The table's accessible name, when no caption is drawn. */
  label?: string;
  /** A visible caption, drawn above the head. */
  caption?: ReactNode;
  children: ReactNode;
}

interface CellProps {
  /** A figure: right-aligned in tabular numerals, so a column of them lines up on the digit. */
  num?: boolean;
  /** Metadata beside the content (an id, a date), set in `--text-muted`. */
  muted?: boolean;
}

export type ThProps = CellProps & Omit<ThHTMLAttributes<HTMLTableCellElement>, "className">;
export type TdProps = CellProps & Omit<TdHTMLAttributes<HTMLTableCellElement>, "className">;

function cellClass({ num, muted }: CellProps): string | undefined {
  const classes = [num === true ? "ui-table-num" : "", muted === true ? "ui-table-muted" : ""].filter(
    (part) => part !== "",
  );
  return classes.length === 0 ? undefined : classes.join(" ");
}

/**
 * A plain data table: hairline rows, a quiet head, nothing else.
 *
 * Compose it from native parts — `<thead>`, `<tbody>`, `<tr>` — with {@link Th} and {@link Td}
 * for the cells that need `num` or `muted`; a cell that needs neither can stay a bare
 * `<th>`/`<td>`, and the stylesheet (`.ui-table`) dresses those too. Kept to this module with its
 * two cells, the exception to one primitive per file `Rows.tsx` records: the classes on a cell
 * mean nothing outside a `.ui-table`.
 *
 * A figure column wants `num` on its `Th` as well as its `Td`, or the heading sits at the other
 * edge from its digits. `className` is not accepted, as for `Button`.
 */
export function Table({ label, caption, children }: TableProps) {
  return (
    <table className="ui-table" aria-label={caption === undefined ? label : undefined}>
      {caption === undefined ? null : <caption className="ui-table-caption">{caption}</caption>}
      {children}
    </table>
  );
}

/** A head cell. `scope="col"` unless the caller says it heads a row. */
export function Th({ num, muted, scope = "col", ...rest }: ThProps) {
  return <th {...rest} scope={scope} className={cellClass({ num, muted })} />;
}

/** A body cell. */
export function Td({ num, muted, ...rest }: TdProps) {
  return <td {...rest} className={cellClass({ num, muted })} />;
}
