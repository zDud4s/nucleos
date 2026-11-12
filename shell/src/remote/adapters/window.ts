/** Browser stand-in for `@tauri-apps/api/window`: an inert window whose methods resolve or no-op. */
export type UnlistenFn = () => void;

export interface Monitor {
  name: string | null;
  scaleFactor: number;
  position: { x: number; y: number };
  size: { width: number; height: number };
  workArea: { position: { x: number; y: number }; size: { width: number; height: number } };
}

const noop: UnlistenFn = () => {};

export class InertWindow {
  readonly label = "remote";
  hide(): Promise<void> {
    return Promise.resolve();
  }
  show(): Promise<void> {
    return Promise.resolve();
  }
  setFocus(): Promise<void> {
    return Promise.resolve();
  }
  /** Undefined: there is no native window to measure (screen-line treats it as "no geometry"). */
  innerPosition(): Promise<{ x: number; y: number } | undefined> {
    return Promise.resolve(undefined);
  }
  scaleFactor(): Promise<number> {
    return Promise.resolve(1);
  }
  onMoved(_handler: (event: { payload: { x: number; y: number } }) => void): Promise<UnlistenFn> {
    return Promise.resolve(noop);
  }
  onResized(_handler: (event: { payload: { width: number; height: number } }) => void): Promise<UnlistenFn> {
    return Promise.resolve(noop);
  }
  onFocusChanged(_handler: (event: { payload: boolean }) => void): Promise<UnlistenFn> {
    return Promise.resolve(noop);
  }
}

export function getCurrentWindow(): InertWindow {
  return new InertWindow();
}

export function currentMonitor(): Promise<Monitor | null> {
  return Promise.resolve(null);
}
