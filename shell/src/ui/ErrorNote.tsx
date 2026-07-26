import type { ReactNode } from "react";

export function ErrorNote({ children }: { children: ReactNode }) {
  return (
    <p className="error" role="alert">
      {children}
    </p>
  );
}
