/* What a share approval binds to, as text a person can read.
 *
 * Approving inside a share records a ledger approval of the canonical hash of ONE
 * `pa_action_drafts` row (contract §3): `payload` (the `item` when it is truthy, else the
 * whole payload), the operator's `edited` patch, `scheduledAt` and `channel`. The sender
 * reads the recipients, cc, reply-to and threading headers out of that payload, so a human
 * who approves must be shown all of it, not only the header line the panel draws
 * (contract §4.2: what is shown is what is bound).
 *
 * This lists every hashed field of the row the approval will carry. The parse mirrors the
 * broker's: a payload or an edit that does not parse has no hash and cannot be approved. */

import type { PaDraftSeen } from '@/lib/tauri-cmd';

export interface BoundField {
	/** `payload` | `edit` | `channel` | `schedule` */
	group: 'payload' | 'edit' | 'channel' | 'schedule';
	/** Dotted path of the value inside its group (`cc[0]`, `headers.reply_to`). */
	path: string;
	/** The value, never truncated: strings verbatim, anything else as JSON. */
	value: string;
}

export type BoundView = { ok: true; fields: BoundField[] } | { ok: false; reason: string };

/** Keys that decide who receives the mail or which thread it joins. Listed first. */
const ROUTING =
	/(^|[._\]])(to|cc|bcc|from|reply_?to|in_?reply_?to|references|recipients?(_?list|_?email)?|emails?|address(es)?)(\[|\.|$)/i;

function flatten(
	group: BoundField['group'],
	value: unknown,
	path: string,
	out: BoundField[],
	depth = 0
): void {
	if (depth > 64) {
		out.push({ group, path, value: '(nested too deeply to show)' });
		return;
	}
	if (Array.isArray(value)) {
		if (value.length === 0) out.push({ group, path, value: '[]' });
		value.forEach((v, i) => {
			flatten(group, v, `${path}[${i}]`, out, depth + 1);
		});
		return;
	}
	if (value !== null && typeof value === 'object') {
		const entries = Object.entries(value as Record<string, unknown>);
		if (entries.length === 0) out.push({ group, path, value: '{}' });
		for (const [k, v] of entries) {
			flatten(group, v, path ? `${path}.${k}` : k, out, depth + 1);
		}
		return;
	}
	out.push({ group, path, value: typeof value === 'string' ? value : JSON.stringify(value) });
}

/** Every field the approval of `seen` binds, or why there is no hash for it. */
export function boundFields(seen: PaDraftSeen | undefined): BoundView {
	if (!seen) return { ok: false, reason: 'This draft carries no row to approve.' };
	let payload: unknown;
	try {
		payload = JSON.parse(seen.payloadJson);
	} catch {
		return {
			ok: false,
			reason: 'The draft’s content could not be read, so it cannot be approved.',
		};
	}
	if (payload === null || typeof payload !== 'object' || Array.isArray(payload)) {
		return {
			ok: false,
			reason: 'The draft’s content could not be read, so it cannot be approved.',
		};
	}
	const item = (payload as { item?: unknown }).item;
	const hashed = item ? item : payload;

	const fields: BoundField[] = [{ group: 'channel', path: 'channel', value: seen.channel }];
	fields.push({ group: 'schedule', path: 'scheduledAt', value: seen.scheduledAt ?? '(none)' });
	const payloadFields: BoundField[] = [];
	flatten('payload', hashed, '', payloadFields);
	const editFields: BoundField[] = [];
	if (seen.editedJson) {
		let edited: unknown;
		try {
			edited = JSON.parse(seen.editedJson);
		} catch {
			return {
				ok: false,
				reason: 'The draft’s edit could not be read, so it cannot be approved.',
			};
		}
		flatten('edit', edited, '', editFields);
		if (editFields.length === 0) editFields.push({ group: 'edit', path: '', value: 'null' });
	}
	const routingFirst = (a: BoundField, b: BoundField) =>
		Number(ROUTING.test(b.path)) - Number(ROUTING.test(a.path));
	return {
		ok: true,
		fields: [
			...fields,
			...[...payloadFields].sort(routingFirst),
			...[...editFields].sort(routingFirst),
		],
	};
}
