import type { ReactNode } from "react";

interface PanelProps {
  title: ReactNode;
  /** Nota pequena ao lado do título. */
  aside?: ReactNode;
  /** true = caixa sólida da camada base (.flat); false = secção sem caixa. */
  flat?: boolean;
  /** Esbatido quando o kill switch global está engatado. */
  dim?: boolean;
  className?: string;
  children: ReactNode;
}

export function Panel({ title, aside, flat = true, dim = false, className, children }: PanelProps) {
  const classes = [flat ? "flat" : null, dim ? "dim" : null, className ?? null]
    .filter(Boolean)
    .join(" ");
  return (
    <section className={classes.length > 0 ? classes : undefined}>
      <h2>
        {title}
        {aside !== undefined && <small>{aside}</small>}
      </h2>
      {children}
    </section>
  );
}
