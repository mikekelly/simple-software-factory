// The standalone terminal page: one session's terminal filling the window.
import { mountTerminal } from "./term-view.js";

const session = new URLSearchParams(location.search).get("session") ?? "";
document.title = `${session} · SSF terminal`;
mountTerminal(document.getElementById("host"), session).focus();
