import { afterEach, vi } from "vitest";
import { configure } from "@testing-library/dom";
import { cleanup } from "@testing-library/react";

// Testing Library gives a `findBy*` query one second to succeed, which is a
// budget for the machine and not for the assertion. Every one of these queries
// waits on react-query handing a mocked answer to a component, and on a loaded
// machine — a full suite is minutes of environment time here — the timers and
// MutationObserver callbacks that drive `waitFor` simply run late. The queries
// that failed that way were correct: their DOM dumps showed the component alive
// and still in its pre-data state, one tick short. Three seconds is slack for
// the scheduler, not permission for a slow assertion.
//
// Configured through `@testing-library/dom` on purpose: `@testing-library/react`
// exports a `configure` of its own that wraps this one to intercept
// `reactStrictMode`, but both write to the single DTL config object that backs
// `screen` — react re-exports `*` from the same instance.
configure({ asyncUtilTimeout: 3000 });

// The wait above only fits if the test is allowed to last long enough to hold
// it. vitest charges its per-test timeout for the whole test, waits included,
// and the default 5 seconds is not enough room on this machine: contention
// dilates wall time roughly fourfold — a test that costs 756 ms in isolation
// was reported at 3026 ms inside a loaded suite — so a dilated test that then
// waits up to 3 seconds goes straight through the ceiling. That the ceiling is
// the real limit and not the query budget is settled by a *synchronous* test,
// with no async query in it at all, having been killed at 5000 ms.
//
// Raised here rather than in `vitest.config.ts` to keep the whole compensation
// for this machine's load in one file, next to the reasoning for it. The two
// numbers are a pair: raising the query budget alone would only trade an
// informative "unable to find" dump for an opaque "test timed out".
vi.setConfig({ testTimeout: 15000 });

// Testing Library only self-registers its unmount hook when the runner injects
// globals, and this suite imports its helpers explicitly. Without this, a
// component left mounted keeps its 3-second poll running into the next test.
afterEach(cleanup);

// jsdom implements no scrolling at all, so `scrollIntoView` is simply absent and calling it throws
// out of the effect that does it. A real WebView2 has it; this keeps the gap in the test environment
// from reading as a fault in the component that follows a conversation downwards.
// Typed through a widened view of the prototype on purpose: the DOM lib declares `scrollIntoView`
// as always present, so an `in` check narrows the negative branch to `never` and fails to compile.
// The absence is a fact about jsdom, not about the type.
const elementProto = Element.prototype as { scrollIntoView?: () => void };
elementProto.scrollIntoView ??= () => {};

// The same kind of gap, one layer up: jsdom implements no layout, so it ships no
// `ResizeObserver`. `cmdk` — the list inside the conversation finder — constructs one on
// mount, and the bare `ReferenceError` that follows unmounts the whole tree into the
// router's error boundary, so the failure reads as "the palette does not render" rather
// than "this environment has no layout".
//
// A stub that observes nothing is the honest shape of it: there are no sizes to report
// here, and a fake that invented some would let a test assert behaviour no browser
// would reproduce.
class NoLayoutResizeObserver {
  observe(): void {}
  unobserve(): void {}
  disconnect(): void {}
}
const withObserver = globalThis as { ResizeObserver?: typeof ResizeObserver };
withObserver.ResizeObserver ??= NoLayoutResizeObserver as unknown as typeof ResizeObserver;
