// Argument parsing for the `host.paActions.*` verbs (G-CONTRACT,
// plans/outbox-redesign/01-plan.md). Pure, so the dispatcher in
// pkg-iframe-host.tsx stays a thin gate + call and the shapes are tested here.
//
// The pkg is not trusted to send well-formed rows: every allowed key is type
// checked (a wrong type is an error, not a silent drop, so a pkg never shows
// "saved" for an edit that was not), and every other key is dropped.

import type { PaActionDraftAttachment, PaActionDraftPatch, PaDraftSeen } from '@/lib/tauri-cmd';

/** Most attachments one edit may carry (G-CONTRACT r2; the Rust store refuses more). */
export const PA_ACTION_MAX_ATTACHMENTS = 20;

type Parsed<T> = { ok: true; value: T } | { ok: false; error: string };

const STRING_KEYS = ['subject', 'body', 'subjectB', 'preheader', 'firstComment'] as const;
const MEDIA_KINDS = new Set(['single', 'carousel', 'thread']);

function isStringArray(v: unknown): v is string[] {
	return Array.isArray(v) && v.every((x) => typeof x === 'string');
}

/** The editable patch of a `host.paActions.update` call: the contract's keys only,
 *  each of its declared type. At least one key must survive. */
export function paActionPatchFromArgs(raw: unknown): Parsed<PaActionDraftPatch> {
	if (raw === null || typeof raw !== 'object' || Array.isArray(raw)) {
		return { ok: false, error: '`patch` must be an object' };
	}
	const src = raw as Record<string, unknown>;
	const patch: PaActionDraftPatch = {};
	for (const key of STRING_KEYS) {
		if (!(key in src)) continue;
		const v = src[key];
		if (typeof v !== 'string') return { ok: false, error: `\`patch.${key}\` must be a string` };
		patch[key] = v;
	}
	if ('hashtags' in src) {
		if (!isStringArray(src.hashtags)) {
			return { ok: false, error: '`patch.hashtags` must be an array of strings' };
		}
		patch.hashtags = [...src.hashtags];
	}
	if ('media' in src) {
		const m = src.media as { kind?: unknown; imageUrls?: unknown } | null;
		if (
			m === null ||
			typeof m !== 'object' ||
			typeof m.kind !== 'string' ||
			!MEDIA_KINDS.has(m.kind) ||
			!isStringArray(m.imageUrls)
		) {
			return {
				ok: false,
				error:
					"`patch.media` must be { kind: 'single' | 'carousel' | 'thread', imageUrls: string[] }",
			};
		}
		patch.media = {
			kind: m.kind as 'single' | 'carousel' | 'thread',
			imageUrls: [...m.imageUrls],
		};
	}
	if ('scheduledIso' in src) {
		const v = src.scheduledIso;
		// The store passes an unparseable time through to `scheduled_at` raw, which the
		// worker's lexical UTC compare cannot read; refuse it here instead.
		if (v !== null && (typeof v !== 'string' || Number.isNaN(Date.parse(v)))) {
			return { ok: false, error: '`patch.scheduledIso` must be an ISO-8601 time or null' };
		}
		patch.scheduledIso = v as string | null;
	}
	if ('attachments' in src) {
		const list = attachmentsFrom(src.attachments);
		if (!list) {
			return {
				ok: false,
				error: `\`patch.attachments\` must be an array of at most ${PA_ACTION_MAX_ATTACHMENTS} { url: string (https), name?: string }`,
			};
		}
		patch.attachments = list;
	}
	if (Object.keys(patch).length === 0) {
		return { ok: false, error: '`patch` has no editable keys' };
	}
	return { ok: true, value: patch };
}

function isHttpsUrl(v: unknown): v is string {
	if (typeof v !== 'string') return false;
	try {
		return new URL(v).protocol === 'https:';
	} catch {
		return false;
	}
}

/** `patch.attachments` (G-CONTRACT r2): each entry's `url` (https) and optional
 *  `name`, every other entry key dropped; `null` when the list is malformed. */
function attachmentsFrom(raw: unknown): PaActionDraftAttachment[] | null {
	if (!Array.isArray(raw) || raw.length > PA_ACTION_MAX_ATTACHMENTS) return null;
	const out: PaActionDraftAttachment[] = [];
	for (const a of raw) {
		if (a === null || typeof a !== 'object' || Array.isArray(a)) return null;
		const { url, name } = a as { url?: unknown; name?: unknown };
		if (!isHttpsUrl(url)) return null;
		if (name !== undefined && typeof name !== 'string') return null;
		out.push(name === undefined ? { url } : { url, name });
	}
	return out;
}

/** The `seen` row of a `host.paActions.commit` call, if any (`undefined` = none). */
export function paDraftSeenFromArgs(raw: unknown): Parsed<PaDraftSeen | undefined> {
	if (raw === undefined || raw === null) return { ok: true, value: undefined };
	const bad = {
		ok: false as const,
		error:
			'`seen` must be { channel: string, payloadJson: string, editedJson: string | null, scheduledAt: string | null }',
	};
	if (typeof raw !== 'object' || Array.isArray(raw)) return bad;
	const s = raw as Record<string, unknown>;
	const strOrNull = (v: unknown) => v === null || v === undefined || typeof v === 'string';
	if (
		typeof s.channel !== 'string' ||
		typeof s.payloadJson !== 'string' ||
		!strOrNull(s.editedJson) ||
		!strOrNull(s.scheduledAt)
	) {
		return bad;
	}
	// Exactly the four hashed columns, as the row carried them: the broker binds the
	// approval to these bytes.
	return {
		ok: true,
		value: {
			channel: s.channel,
			payloadJson: s.payloadJson,
			editedJson: (s.editedJson as string | null | undefined) ?? null,
			scheduledAt: (s.scheduledAt as string | null | undefined) ?? null,
		},
	};
}

/** The optional reason of a `host.paActions.reject` call. */
export function rejectReasonFromArgs(raw: unknown): Parsed<string | undefined> {
	if (raw === undefined || raw === null) return { ok: true, value: undefined };
	if (typeof raw !== 'string') return { ok: false, error: '`reason` must be a string' };
	const t = raw.trim();
	return { ok: true, value: t === '' ? undefined : t };
}

/** The text to show a person for a failed list / edit / approve / reject. Share
 *  refusals arrive as `<code>: <sentence>`; show the sentence. Returned to the pkg as
 *  `structuredContent.message` next to the raw `error`. */
export function draftActionErrorMessage(err: unknown): string {
	const raw = err instanceof Error ? err.message : typeof err === 'string' ? err : '';
	// The browser's own words for "no answer came back" mean nothing to a reviewer, and they do
	// not say whether the request landed: the reply may be what was lost.
	if (
		/^(Failed to fetch|NetworkError when attempting to fetch resource\.?|Load failed|Network request failed)$/i.test(
			raw.trim()
		)
	) {
		return "Couldn't reach the box (network error). Check your connection and try again. If you are unsure whether it went through, reload the list first.";
	}
	const http = /^HTTP RPC error: (\d{3})\b/.exec(raw);
	if (http) {
		return `The box did not answer properly (HTTP ${http[1]}). Try again in a moment; reload the list first if you are unsure whether it went through.`;
	}
	// A refused edit of a social_queue post (G-CONTRACT r2): `social_refused: <code>: <why>`
	// (one `<code>: <why>` per problem, `; `-separated), behind the daemon's
	// `pa_actions_update: ` when it came over RPC. Show the reasons.
	const social = /^(?:pa_actions_update: )?social_refused: (.+)$/s.exec(raw);
	if (social) {
		const reasons = social[1]
			.split('; ')
			.map((p) => p.replace(/^[a-z0-9_]+: /, '').trim())
			.filter(Boolean)
			.map((p) => (/[.!?]$/.test(p) ? p : `${p}.`));
		return `This post can't be saved: ${reasons.join(' ')} Nothing was changed.`;
	}
	const m =
		/^(?:approval_unavailable|approval_refused|draft_changed|forbidden|invalid_request): (.+)$/s.exec(
			raw
		);
	const text = (m ? m[1] : raw).trim();
	return text || 'The action failed. Nothing was changed.';
}

/** The problem codes of a refused social_queue post edit (G-CONTRACT r2), in order, or
 *  `[]` when the error is not one. */
export function socialRefusalCodes(err: unknown): string[] {
	const raw = err instanceof Error ? err.message : typeof err === 'string' ? err : '';
	const m = /^(?:pa_actions_update: )?social_refused: (.+)$/s.exec(raw);
	if (!m) return [];
	return m[1]
		.split('; ')
		.map((p) => /^([a-z0-9_]+): /.exec(p)?.[1])
		.filter((c): c is string => !!c);
}
