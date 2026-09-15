/**
 * SSF's out-of-band delivery bridge for Pi and Oh My Pi.
 *
 * The daemon writes one JSON file per item event.  The extension queues it as
 * a user-attributed context message that triggers a turn, then acknowledges
 * the file. No bytes pass through the terminal composer.
 */
import * as fs from "node:fs";
import * as path from "node:path";

export default function (pi: any) {
	let timer: ReturnType<typeof setInterval> | undefined;
	let polling = false;
	let sessionManager: any;
	const mailbox = process.env.SSF_DELIVERY_MAILBOX;
	const ready = mailbox ? path.join(mailbox, "ready.json") : undefined;

	async function poll() {
		if (!mailbox || polling) return;
		polling = true;
		try {
			const entries = fs
				.readdirSync(mailbox)
				.filter((name) => name.endsWith(".json") && name !== "ready.json")
				.sort();
			for (const name of entries) {
				const pending = path.join(mailbox, name);
				const message = JSON.parse(fs.readFileSync(pending, "utf8"));
				if (typeof message.text !== "string" || message.text.length === 0) continue;
				const recorded = sessionManager?.getEntries().some(
					(entry: any) =>
						entry.type === "custom_message" &&
						entry.customType === "ssf-item-activity" &&
						entry.details?.deliveryId === name,
				);
				if (recorded) {
					fs.renameSync(pending, `${pending}.ack`);
					continue;
				}
				// A custom message does not submit or replace the interactive editor.
				// triggerTurn wakes an idle agent; followUp queues behind an active turn.
				pi.sendMessage(
					{
						customType: "ssf-item-activity",
						content: message.text,
						display: true,
						attribution: "user",
						details: { deliveryId: name },
					},
					{ deliverAs: "followUp", triggerTurn: true },
				);
				fs.renameSync(pending, `${pending}.ack`);
			}
		} catch {
			// A partial write, shutdown race, or transient send error remains pending.
		} finally {
			polling = false;
		}
	}

	pi.on("session_start", async (_event: any, ctx: any) => {
		if (!mailbox || !ready) return;
		sessionManager = ctx.sessionManager;
		fs.mkdirSync(mailbox, { recursive: true, mode: 0o700 });
		fs.writeFileSync(ready, JSON.stringify({ pid: process.pid }), { mode: 0o600 });
		await poll();
		timer = setInterval(poll, 100);
	});

	pi.on("session_shutdown", async () => {
		if (timer) clearInterval(timer);
		if (!ready) return;
		try {
			const owner = JSON.parse(fs.readFileSync(ready, "utf8"));
			if (owner.pid === process.pid) fs.unlinkSync(ready);
		} catch {
			// Already gone or replaced by another process.
		}
	});
}
