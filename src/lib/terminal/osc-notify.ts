// OSC (Operating System Command) notification observer for terminal panes.
//
// Watches the PTY data stream for three escape-sequence formats and fires
// a desktop notification when one is matched. Does NOT strip the sequence
// from the byte stream — xterm.js's own state machine silently ignores
// unrecognized OSCs, so we get to be a passive observer.
//
// Phase 8 of the projects-first-class plan. Mirrors `soloterm/notify` (MIT)
// for protocol compatibility: any script that fires notifications into
// iTerm2 / Ghostty / Kitty will also fire them in Ikenga.
//
// Supported sequences (BEL = \x07, ST = \x1b\\):
//   OSC 9   — `\e]9;<body><BEL>`                 (iTerm2; body only)
//   OSC 777 — `\e]777;notify;<title>;<body><BEL>` (rxvt; title + body)
//   OSC 99  — `\e]99;i=<id>:p=body;<body><BEL>`   (Kitty; multi-part by id)
//             `\e]99;i=<id>:p=title;<title><BEL>`
//
// The parser is stateful — escape sequences span chunks routinely.

import {
	isNotificationPermissionGranted,
	requestNotificationPermission,
	sendDesktopNotification,
} from '@/lib/transport/shims';

interface ParseState {
	/** Buffer of currently-accumulating OSC payload (between `\e]` and terminator). */
	buf: string;
	/** Are we currently inside an OSC sequence? */
	inOsc: boolean;
	/** Did the previous byte end with ESC (so a `\\` here means ST)? */
	pendingSt: boolean;
	/** Kitty OSC 99 part-buffer keyed by id: parts come as separate OSCs and merge. */
	kittyParts: Map<string, { title?: string; body?: string }>;
}

export interface OscNotification {
	title: string;
	body: string;
}

const decoder = new TextDecoder('utf-8', { fatal: false });

export function createOscObserver(opts: { onNotify: (n: OscNotification) => void }) {
	const state: ParseState = {
		buf: '',
		inOsc: false,
		pendingSt: false,
		kittyParts: new Map(),
	};

	function flush(payload: string) {
		const note = parseOscPayload(payload, state.kittyParts);
		if (note) opts.onNotify(note);
	}

	function feed(bytes: Uint8Array) {
		// Decode in stream-friendly way (chunked UTF-8 is rare in escape sequences
		// since they're all ASCII inside, but pass `stream: true` for safety).
		const chunk = decoder.decode(bytes, { stream: true });
		for (let i = 0; i < chunk.length; i++) {
			const ch = chunk[i];
			if (!state.inOsc) {
				// Watch for ESC ].
				if (ch === '\x1b') {
					// Peek next char.
					if (i + 1 < chunk.length && chunk[i + 1] === ']') {
						state.inOsc = true;
						state.buf = '';
						state.pendingSt = false;
						i += 1;
					}
				}
				continue;
			}
			// Inside OSC. Watch for terminator.
			if (ch === '\x07') {
				flush(state.buf);
				state.inOsc = false;
				state.buf = '';
				state.pendingSt = false;
				continue;
			}
			if (state.pendingSt) {
				state.pendingSt = false;
				if (ch === '\\') {
					flush(state.buf);
					state.inOsc = false;
					state.buf = '';
					continue;
				}
				// false alarm — the ESC was a stray. Treat as part of body.
				state.buf += '\x1b';
				// fall through to push ch
			}
			if (ch === '\x1b') {
				state.pendingSt = true;
				continue;
			}
			// Cap the buffer to defend against pathological input.
			if (state.buf.length < 4096) {
				state.buf += ch;
			} else if (state.buf.length === 4096) {
				// Drop the sequence — abort accumulation.
				state.inOsc = false;
				state.buf = '';
				state.pendingSt = false;
			}
		}
	}

	function reset() {
		state.buf = '';
		state.inOsc = false;
		state.pendingSt = false;
		state.kittyParts.clear();
	}

	return { feed, reset };
}

function parseOscPayload(
	payload: string,
	kittyParts: Map<string, { title?: string; body?: string }>
): OscNotification | null {
	// payload looks like "9;Build complete" or "777;notify;Title;Body" or
	// "99;i=1:p=body;Hello".
	const semi = payload.indexOf(';');
	if (semi < 0) return null;
	const op = payload.slice(0, semi);
	const rest = payload.slice(semi + 1);

	if (op === '9') {
		// iTerm2: body only, no title.
		if (!rest) return null;
		return { title: 'Ikenga', body: rest };
	}

	if (op === '777') {
		// rxvt: "notify;<title>;<body>". The subtype is "notify".
		const subSemi = rest.indexOf(';');
		if (subSemi < 0) return null;
		const subtype = rest.slice(0, subSemi);
		if (subtype !== 'notify') return null;
		const afterSubtype = rest.slice(subSemi + 1);
		const titleEnd = afterSubtype.indexOf(';');
		if (titleEnd < 0) {
			return { title: afterSubtype, body: '' };
		}
		return {
			title: afterSubtype.slice(0, titleEnd),
			body: afterSubtype.slice(titleEnd + 1),
		};
	}

	if (op === '99') {
		// Kitty: "i=<id>:p=<part>;<content>" — multi-part. Same id+different
		// p value merge into one notification.
		const headerEnd = rest.indexOf(';');
		if (headerEnd < 0) return null;
		const header = rest.slice(0, headerEnd);
		const content = rest.slice(headerEnd + 1);
		const params = new Map<string, string>();
		for (const kv of header.split(':')) {
			const eq = kv.indexOf('=');
			if (eq > 0) params.set(kv.slice(0, eq), kv.slice(eq + 1));
		}
		const id = params.get('i') ?? '_';
		const part = params.get('p') ?? 'body';
		const slot = kittyParts.get(id) ?? {};
		if (part === 'title') slot.title = content;
		else slot.body = content;
		kittyParts.set(id, slot);
		// Fire when we have at least a body. If a title arrives later for the
		// same id, the body-only notification has already fired — acceptable
		// for v1.
		if (slot.body !== undefined) {
			kittyParts.delete(id);
			return { title: slot.title ?? 'Ikenga', body: slot.body };
		}
		return null;
	}

	return null;
}

/** Convenience helper for callers: ensure permission, then send. */
export async function fireOscNotification(note: OscNotification): Promise<void> {
	try {
		let granted = await isNotificationPermissionGranted();
		if (!granted) {
			granted = (await requestNotificationPermission()) === 'granted';
		}
		if (!granted) return;
		await sendDesktopNotification({ title: note.title, body: note.body });
	} catch (err) {
		// Notification rejection shouldn't crash the terminal pane.
		console.warn('[osc-notify] sendNotification failed:', err);
	}
}
