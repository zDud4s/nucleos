import ReactDOM from "react-dom/client";
import "../fonts.css";
import "../tokens.css";
import "../tailwind.css";
import "../base.css";
import "../ui.css";
import "../app.css";
import "./remote.css";
import { createAppQueryClient } from "../app/queryClient";
import { setTransport } from "../data/transport";
import { adoptStyleNonce } from "../lib/style-nonce";
import { installDropGuard } from "./drop-guard";
import { remoteTransport } from "./transport";
import { startRemoteApp } from "./RemoteApp";
import { TicketStepUp } from "./TicketStepUp";

// Same order and reasons as `src/main.tsx`; the transport is set before anything can fetch.
setTransport(remoteTransport);
adoptStyleNonce();
// A file dropped outside a drop zone must not navigate the tab away from the app, on any page.
installDropGuard();

const queryClient = createAppQueryClient();
const root = ReactDOM.createRoot(document.getElementById("root") as HTMLElement);

if (window.location.pathname === "/stepup") root.render(<TicketStepUp />);
else startRemoteApp(root, queryClient, (r, qc) => {
  void import("../app/main-window").then(({ mountMainWindow }) => mountMainWindow(r, qc));
});
