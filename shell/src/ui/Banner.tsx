import type { ReactNode } from "react";

/** Banner de exceção — camada elevada (vidro). Domina o ecrã sem histeria. */
export function Banner({
  tone,
  title,
  action,
  children,
}: {
  tone: "kill" | "budget";
  title: ReactNode;
  action?: ReactNode;
  children?: ReactNode;
}) {
  return (
    <div className={`banner banner--${tone}`}>
      <div>
        <span className="b-title">{title}</span>
        {children !== undefined && <p>{children}</p>}
      </div>
      {action}
    </div>
  );
}
