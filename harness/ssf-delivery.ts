/**
 * SSF's out-of-band delivery bridge for Pi and Oh My Pi.
 *
 * The daemon writes one JSON file per item event.  The extension injects it as
 * a user-attributed context message, mid-turn when the harness can do that
 * without interrupting the agent, and acknowledges the file once the session
 * transcript records that message.  An acknowledgement therefore means the
 * agent's own record of the event exists -- not that the harness accepted a
 * call it could still drop.  Until the record is there the file stays pending,
 * which is what the daemon retries and what a relaunch reconciles (#390).
 * No bytes pass through the terminal composer.
 */
import * as fs from "node:fs";
import * as path from "node:path";

/** The one entry kind this bridge reads back: the injected activity message. */
interface TranscriptEntry {
	type?: string;
	customType?: string;
	details?: { deliveryId?: string };
}

/** The session's transcript, as the extension API exposes it. */
interface SessionManager {
	getEntries(): TranscriptEntry[];
}

interface SessionContext {
	sessionManager: SessionManager;
}

/** The slice of the Pi/OMP extension API the bridge uses. */
interface Harness {
	sendMessage(
		message: {
			customType: string;
			content: string;
			display: boolean;
			attribution: string;
			details: { deliveryId: string };
		},
		options: { deliverAs: string; triggerTurn: boolean },
	): void;
	on(
		event: "session_start" | "session_shutdown",
		handler: (event: unknown, ctx: SessionContext) => Promise<void>,
	): void;
}

/** The event's text, when the mailbox file holds one, from unvalidated JSON. */
function eventText(body: string): string | undefined {
	const parsed: unknown = JSON.parse(body);
	if (parsed === null || typeof parsed !== "object" || !("text" in parsed)) return;
	const text = parsed.text;
	if (typeof text !== "string" || text.length === 0) return;
	return text;
}

export default function (pi: Harness) {
	let timer: ReturnType<typeof setInterval> | undefined;
	let polling = false;
	let sessionManager: SessionManager | undefined;
	// Events handed to this process's harness whose record is not in the
	// session yet.  The injection is at the next agent step boundary, so a
	// busy agent records it only when the tool calls in flight have finished;
	// until then the same file must not be handed over twice.  A new session
	// starts with an empty transcript and takes them again (#390).
	const handed = new Set<string>();
	const mailbox = process.env.SSF_DELIVERY_MAILBOX;
	const ready = mailbox ? path.join(mailbox, "ready.json") : undefined;
	// `followUp` waits for the current turn to end, which an agent inside one long
	// tool loop may not reach for hours, and the file is acknowledged either way
	// (#385). OMP's `aside` is injected at the next agent step boundary without
	// interrupting the tool batch in flight; Pi's extension API has no `aside`
	// (`steer` | `followUp` | `nextTurn`, with an unknown mode treated as a
	// steer), but its `steer` means that same step boundary, so Pi names it
	// explicitly. A harness the launcher does not name gets `steer` too: on OMP
	// that interrupts the step the event arrives in and finishes it in the
	// background, which beats a queue that may never drain.
	const deliverAs = process.env.SSF_HARNESS === "omp" ? "aside" : "steer";

	// Does the session transcript already hold this delivery?  The entry
	// carries the mailbox file's name in extension-only details, and is
	// written when the message reaches the conversation, so it is the one
	// record that means the agent has it.
	function recorded(entries: readonly TranscriptEntry[] | undefined, name: string) {
		return (
			entries?.some(
				(entry) =>
					entry.type === "custom_message" &&
					entry.customType === "ssf-item-activity" &&
					entry.details?.deliveryId === name,
			) === true
		);
	}

	async function poll() {
		if (!mailbox || polling) return;
		polling = true;
		try {
			const pending = fs
				.readdirSync(mailbox)
				.filter((name) => name.endsWith(".json") && name !== "ready.json")
				.sort();
			// One read of the transcript per poll, however many events are
			// waiting on it: the session manager copies its entries out.
			const entries = sessionManager?.getEntries();
			for (const name of pending) {
				const file = path.join(mailbox, name);
				if (recorded(entries, name)) {
					fs.renameSync(file, `${file}.ack`);
					handed.delete(name);
					continue;
				}
				if (handed.has(name)) continue;
				const text = eventText(fs.readFileSync(file, "utf8"));
				if (text === undefined) continue;
				// A custom message does not submit or replace the interactive editor.
				// triggerTurn wakes an idle agent; a busy one takes the mode above.
				// The file is left pending: only the record above acknowledges it.
				pi.sendMessage(
					{
						customType: "ssf-item-activity",
						content: text,
						display: true,
						attribution: "user",
						details: { deliveryId: name },
					},
					{ deliverAs, triggerTurn: true },
				);
				handed.add(name);
			}
		} catch {
			// A partial write, shutdown race, or transient send error remains pending.
		} finally {
			polling = false;
		}
	}

	pi.on("session_start", async (_event, ctx) => {
		if (!mailbox || !ready) return;
		// A session starting where an earlier one did not shut down first
		// (a replaced or restarted session) takes over the poller; two of
		// them could hand the same pending event over twice.
		clearInterval(timer);
		sessionManager = ctx.sessionManager;
		// A session starting here has a transcript of its own: what an earlier
		// one in this process was handed is not in it, so those files are its
		// to take.
		handed.clear();
		fs.mkdirSync(mailbox, { recursive: true, mode: 0o700 });
		fs.writeFileSync(ready, JSON.stringify({ pid: process.pid }), { mode: 0o600 });
		await poll();
		timer = setInterval(poll, 100);
	});

	pi.on("session_shutdown", async () => {
		clearInterval(timer);
		if (!ready) return;
		try {
			const owner: unknown = JSON.parse(fs.readFileSync(ready, "utf8"));
			if (
				owner !== null &&
				typeof owner === "object" &&
				"pid" in owner &&
				owner.pid === process.pid
			) {
				fs.unlinkSync(ready);
			}
		} catch {
			// Already gone or replaced by another process.
		}
	});
}
