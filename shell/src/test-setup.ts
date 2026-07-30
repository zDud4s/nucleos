import { afterEach } from "vitest";
import { cleanup } from "@testing-library/react";

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
