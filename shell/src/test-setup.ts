import { afterEach } from "vitest";
import { cleanup } from "@testing-library/react";

// Testing Library only self-registers its unmount hook when the runner injects
// globals, and this suite imports its helpers explicitly. Without this, a
// component left mounted keeps its 3-second poll running into the next test.
afterEach(cleanup);
