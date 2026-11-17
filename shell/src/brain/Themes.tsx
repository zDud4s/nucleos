import { useState } from "react";
import { useThemes } from "../data/themes";
import { ThemesAccepted } from "./ThemesAccepted";
import { ThemesProposed } from "./ThemesProposed";
import "./ThemesProposed.css";

/**
 * One compact "Themes" block: a header line with the counts and a toggle, closed by default. Open,
 * it lists the proposed themes and then the accepted ones, one row each. Rows mount only while
 * open, so a closed block costs no per-theme fetch. Nothing renders when there are no themes.
 */
export function Themes() {
  const proposed = useThemes("proposed");
  const accepted = useThemes("accepted");
  const [open, setOpen] = useState(false);
  const np = proposed.data?.length ?? 0;
  const na = accepted.data?.length ?? 0;
  if (np + na === 0 && !proposed.isError) return null;
  const summary = np > 0 ? `Themes ${na} · ${np} proposed` : `Themes ${na}`;
  return (
    <section className="themes-block" aria-label="Themes">
      <button
        type="button"
        className="themes-toggle"
        aria-expanded={open}
        onClick={() => setOpen((v) => !v)}
      >
        <span aria-hidden="true">{open ? "▾" : "▸"}</span> {summary}
      </button>
      {open && (
        <>
          <ThemesProposed />
          <ThemesAccepted />
        </>
      )}
    </section>
  );
}
