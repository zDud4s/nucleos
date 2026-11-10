Review patterns for TypeScript/React diffs and the Tauri shell (`shell/`): bugs checkable by reading.

- Stale closure: effect/callback/timer reads state or props not in the deps array. Tell: `useEffect(..., [])` whose body uses a prop; `setInterval` reading state; `useCallback` with a missing dep.
- Wrong deps: object/array/function literal in deps (re-fires every render), or a dep deliberately omitted with an eslint-disable comment.
- Effect without cleanup: `addEventListener`, `setInterval`/`setTimeout`, subscriptions, or Tauri `listen()` with no returned unsubscribe/`unlisten`. Tell: `listen(...)` inside an effect that does not await and call the returned fn on cleanup (watch the race where cleanup runs before the promise resolves).
- setState after unmount / fetch without abort: async work in an effect with no `AbortController` or `cancelled` flag.
- Response race: two overlapping requests (typing, route change, refetch) and the older one resolves last. Tell: no request id, abort, or "is still current" check before `setState`.
- Derived state in an effect (`useEffect(() => setX(f(y)), [y])`): report only if it causes a concrete defect in the changed code (stale frame, loop).
- In-place mutation: `state.push`, `arr.sort()` or `obj.x = ...` on state/props, then `setState(sameRef)`. Tell: no re-render.
- Index keys: `key={i}`: report only if it causes a concrete defect in the changed code (list reorders, filters or holds local state).
- Promises: missing `await`, floating promise with no `.catch`, `async` callback in `forEach` (use `for...of` or `Promise.all`), `async` handler whose errors are never surfaced.
- Type escapes: `as X`, `as unknown as`, `any`, `!` on a value that can really be undefined (`find()`, `Map.get`, `querySelector`, `invoke` results).
- Silenced bugs: `?.` or `?? default` on a value that should always exist, hiding a wrong shape. Empty `catch {}`.
- Missing states: a new fetch with no loading, empty or error rendering.
- Double submit: submit/click handler with no in-flight guard or disabled button.
- Accessibility (report only if it causes a concrete defect in the changed code): icon-only button without `aria-label`; `div onClick` without role/tabIndex/key handler; input without a label; `outline: none`.
- Tests (React Testing Library): asserts only that it renders; `setTimeout`/fixed sleeps instead of `findBy*`/`waitFor`; mocks whose shape differs from the real API/command payload; snapshot-only; no failure-path case.
- Tauri `invoke`: command name and arg keys must match the Rust `#[tauri::command]` (Grep it). Tell: camelCase JS arg vs snake_case Rust param (Tauri converts by default, check it is not overridden), a missing arg, a result type differing from the Rust return.
- `dangerouslySetInnerHTML` or `innerHTML` with agent/email/web content is untrusted HTML: needs sanitising.
- Shared CSS: an edited or renamed class may be used by other pages; Grep it across `shell/src`.
