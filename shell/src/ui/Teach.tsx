import type { ReactNode } from "react";

/** Empty state que ensina o modelo mental em vez de dizer "nothing here". */
export function Teach({ title, children }: { title: ReactNode; children: ReactNode }) {
  return (
    <div className="teach">
      <span className="t-title">{title}</span>
      {children}
    </div>
  );
}
