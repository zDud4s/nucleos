import { createContext, useCallback, useContext, useEffect, useState, type ReactNode } from "react";

/**
 * Native browser webviews paint above the DOM, so anything drawn over them (a modal, a menu, the
 * palette, a drawer) is hidden behind them. Each DOM overlay reports itself here while it is open;
 * the browser page hides its webviews while `useOverlayOpen()` is true.
 */
type Register = () => () => void;

// Two contexts: the count changes often, while `register` must keep one identity for the provider's
// whole life so reporters' effects do not unregister/register on every count change.
const CountContext = createContext<number | null>(null);
const RegisterContext = createContext<Register | null>(null);

export function OverlayProvider({ children }: { children?: ReactNode }) {
  const [count, setCount] = useState(0);
  const register = useCallback<Register>(() => {
    let live = true;
    setCount((n) => n + 1);
    return () => {
      if (!live) return;
      live = false;
      setCount((n) => n - 1);
    };
  }, []);
  return (
    <RegisterContext.Provider value={register}>
      <CountContext.Provider value={count}>{children}</CountContext.Provider>
    </RegisterContext.Provider>
  );
}

/** True while at least one overlay is open. Outside a provider: always false. */
export function useOverlayOpen(): boolean {
  const count = useContext(CountContext);
  return count !== null && count > 0;
}

/** The provider's stable `register` function, or undefined outside a provider. */
export function useOverlayRegister(): Register | undefined {
  return useContext(RegisterContext) ?? undefined;
}

/** Registers one overlay for as long as `open` is true, and on unmount. No-op outside a provider. */
export function useReportOverlay(open: boolean): void {
  const register = useOverlayRegister();
  useEffect(() => {
    if (!open || register === undefined) return undefined;
    return register();
  }, [open, register]);
}

/**
 * Renders nothing; reports an overlay while mounted. For Radix content components, which only mount
 * while open, so controlled and uncontrolled roots are covered alike.
 */
export function OverlayReporter() {
  useReportOverlay(true);
  return null;
}
