/** Browser stand-in for `@tauri-apps/api/event`: nothing is ever delivered, nothing fails. */
export type UnlistenFn = () => void;

export interface Event<T> {
  event: string;
  id: number;
  payload: T;
}

const noop: UnlistenFn = () => {};

export function listen<T>(
  _event: string,
  _handler: (event: Event<T>) => void,
  _options?: unknown,
): Promise<UnlistenFn> {
  return Promise.resolve(noop);
}

export function once<T>(
  _event: string,
  _handler: (event: Event<T>) => void,
  _options?: unknown,
): Promise<UnlistenFn> {
  return Promise.resolve(noop);
}

export function emit(_event: string, _payload?: unknown): Promise<void> {
  return Promise.resolve();
}
