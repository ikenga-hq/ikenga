// D-05 `audit` (`designs/people.html` §7 "audit") — the pure half of the
// Audit tab (WP-77, G-ACCESS §6): row shape, the action / who / device /
// when columns, the filter chips and the scope → `access_audit_list` filter.
//
// The rows come from `access_audit_list` (§9.1); the daemon / broker decides
// what the caller may see (§6.7). Everything here is presentation.

import { TIER_LABELS } from '@/lib/access/caps.gen';
import type { AccessStatus, AuditFilter } from '@/lib/access/client';

export type AuditCategory = NonNullable<AuditFilter['category']>;

/** One `access_audit_list` row (camelCase; `detail` parsed). */
export interface AuditRow {
	seq: number;
	atMs: number;
	kind: string;
	category: AuditCategory;
	principalId: string | null;
	actorName: string | null;
	deviceId: string | null;
	deviceName: string | null;
	via: 'session' | 'device' | 'operator' | 'cli' | 'system';
	subjectPrincipalId: string | null;
	subjectName: string | null;
	subjectDeviceId: string | null;
	subjectDeviceName: string | null;
	projectKey: string | null;
	target: string | null;
	remoteAddr: string | null;
	userAgent: string | null;
	detail: Record<string, unknown> | null;
}

export interface AuditPage {
	rows: AuditRow[];
	nextBefore: number | null;
}

/** D-05 `AUDIT_KINDS` (the Kind chips), in the design's order. */
export const AUDIT_CATEGORIES: ReadonlyArray<{ id: AuditCategory; label: string }> = [
	{ id: 'permission', label: 'Permissions' },
	{ id: 'dispatch', label: 'Dispatch' },
	{ id: 'access', label: 'Access' },
	{ id: 'pairing', label: 'Pairing' },
	{ id: 'people', label: 'People' },
];

/** The Action column (§6.5's closed kind list, in D-05's words). */
const ACTION_LABELS: Readonly<Record<string, string>> = {
	'pair.started': 'Started pairing',
	'pair.failed': 'Pairing failed',
	'pair.denied': 'Denied pairing',
	'pair.allowed': 'Paired device',
	'pair.cancelled': 'Cancelled pairing',
	'device.tier_changed': 'Changed device capability',
	'device.revoked': 'Revoked device',
	'device.expired': 'Device expired',
	'routing.changed': 'Changed approval policy',
	'member.added': 'Added member',
	'member.role_changed': 'Changed role',
	'member.removed': 'Removed member',
	'member.restored': 'Restored member',
	'member.expired': 'Membership expired',
	'invite.issued': 'Created invite',
	'invite.revoked': 'Revoked invite',
	'invite.accepted': 'Accepted invite',
	'policy.changed': 'Changed role policy',
	'policy.owner_approval_changed': 'Changed Owner approval',
	'ownership.offered': 'Offered ownership',
	'ownership.accepted': 'Accepted ownership',
	'permission.decided': 'Allowed permission',
	'permission.refused': 'Refused permission',
	'dispatch.sent': 'Dispatched',
	'auth.login_ok': 'Signed in',
	'auth.login_fail': 'Sign-in failed',
	'auth.login_throttled': 'Sign-in paused',
	'auth.logout': 'Signed out',
	'auth.password_changed': 'Changed password',
	'auth.account_created': 'Created account',
	'auth.account_disabled': 'Disabled account',
	'auth.account_enabled': 'Enabled account',
	'auth.sessions_revoked': 'Signed out everywhere',
	'auth.provision_failed': 'Account setup failed',
	'auth.probe_failed': 'Server check failed',
	'share.artifact_viewed': 'Viewed artifact',
	'app.locked': 'App locked',
	'app.unlocked': 'App unlocked',
	'vault.locked': 'Vault locked',
	'vault.unlocked': 'Vault unlocked',
	'store.created': 'Created the access store',
	'audit.verified': 'Verified the audit log',
	'audit.exported': 'Exported the audit log',
	'audit.chain_broken': 'Audit chain broken',
	'audit.resealed': 'Resealed the audit chain',
	'secrets.kek_rotated': 'Rotated the secrets key',
	'server.update_requested': 'Requested a server update',
};

/** The Action column for one row. */
export function actionLabel(row: Pick<AuditRow, 'kind' | 'detail'>): string {
	if (row.kind === 'permission.decided' && row.detail?.decision === 'deny') {
		return 'Denied permission';
	}
	return ACTION_LABELS[row.kind] ?? row.kind;
}

/** First 8 chars of an id, for a principal with no name on record. */
function shortId(id: string): string {
	return id.slice(0, 8);
}

/** The Who column: the actor (§6.1 `principal_id`). A root-CLI row
 *  (`via = cli`) keeps the account in `principal_id` for the G-PRINCIPAL
 *  §6.1 view, but its actor is the server operator (review m-3). */
export function whoLabel(row: AuditRow): string {
	if (row.via === 'cli') return 'server operator';
	if (row.actorName) return row.actorName;
	if (row.principalId) return shortId(row.principalId);
	return 'system';
}

/** "Firefox" / "Chrome" / "Safari" / "Edge" from a User-Agent, else "browser". */
export function uaFamily(ua: string | null): string {
	if (!ua) return 'browser';
	if (/Edg\//.test(ua)) return 'Edge';
	if (/Firefox\//.test(ua)) return 'Firefox';
	if (/Chrome\//.test(ua)) return 'Chrome';
	if (/Safari\//.test(ua)) return 'Safari';
	return 'browser';
}

/** The Device column: the actor's device; a T1 password session reads
 *  "Browser session · <UA family> · <session_ref>" (review C-19). */
export function deviceLabel(row: AuditRow): string {
	if (row.via === 'cli') return 'server CLI';
	if (row.deviceName) return row.deviceName;
	if (row.via === 'session') {
		const ref = typeof row.detail?.session_ref === 'string' ? row.detail.session_ref : null;
		return ['Browser session', uaFamily(row.userAgent), ref].filter(Boolean).join(' · ');
	}
	if (row.deviceId) return shortId(row.deviceId);
	return '—';
}

/** The Target column: the row's display target, else who / what it is about. */
export function targetLabel(row: AuditRow): string {
	return row.target ?? row.subjectName ?? row.subjectDeviceName ?? '—';
}

function pad(n: number): string {
	return String(n).padStart(2, '0');
}

const MONTHS = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'];

/** The When column, as D-05 draws it: `10:42`, `Yest 14:05`, `Sep 18 09:00`. */
export function whenLabel(atMs: number, now: number): string {
	const at = new Date(atMs);
	const today = new Date(now);
	const hm = `${pad(at.getHours())}:${pad(at.getMinutes())}`;
	const startOfToday = new Date(today.getFullYear(), today.getMonth(), today.getDate()).getTime();
	if (atMs >= startOfToday) return hm;
	if (atMs >= startOfToday - 86_400_000) return `Yest ${hm}`;
	return `${MONTHS[at.getMonth()]} ${at.getDate()} ${hm}`;
}

/** The chip filters (D-05 `S.auditFilter`), applied to the loaded rows. */
export interface ChipFilter {
	/** A principal id (actor or subject), or `all`. */
	who: string;
	/** A device id (actor's or subject), or `all`. */
	device: string;
	kind: AuditCategory | 'all';
	q: string;
}

export const NO_CHIPS: ChipFilter = { who: 'all', device: 'all', kind: 'all', q: '' };

/** Whether a row passes the chips — the same reading `access_audit_list`'s
 *  filter has (who / device match the actor **or** the subject). */
export function matchesChips(row: AuditRow, f: ChipFilter): boolean {
	if (f.who !== 'all' && row.principalId !== f.who && row.subjectPrincipalId !== f.who) {
		return false;
	}
	if (f.device !== 'all' && row.deviceId !== f.device && row.subjectDeviceId !== f.device) {
		return false;
	}
	if (f.kind !== 'all' && row.category !== f.kind) return false;
	const q = f.q.trim().toLowerCase();
	if (!q) return true;
	const hay = [
		actionLabel(row),
		row.kind,
		targetLabel(row),
		whoLabel(row),
		deviceLabel(row),
		row.detail ? JSON.stringify(row.detail) : '',
	]
		.join(' ')
		.toLowerCase();
	return hay.includes(q);
}

export interface ChipOption {
	id: string;
	label: string;
	count: number;
}

/** The Who chips: every actor on the loaded rows, most active first. */
export function whoOptions(rows: readonly AuditRow[]): ChipOption[] {
	const by = new Map<string, ChipOption>();
	for (const r of rows) {
		// A root-CLI row's `principal_id` is the account it acted on.
		if (!r.principalId || r.via === 'cli') continue;
		const o = by.get(r.principalId) ?? { id: r.principalId, label: whoLabel(r), count: 0 };
		o.count += 1;
		by.set(r.principalId, o);
	}
	return [...by.values()].sort((a, b) => b.count - a.count || a.label.localeCompare(b.label));
}

/** The Device chips: every actor device on the loaded rows. */
export function deviceOptions(rows: readonly AuditRow[]): ChipOption[] {
	const by = new Map<string, ChipOption>();
	for (const r of rows) {
		if (!r.deviceId) continue;
		const o = by.get(r.deviceId) ?? { id: r.deviceId, label: deviceLabel(r), count: 0 };
		o.count += 1;
		by.set(r.deviceId, o);
	}
	return [...by.values()].sort((a, b) => b.count - a.count || a.label.localeCompare(b.label));
}

/** Rows per category, for the Kind chips' counts. */
export function categoryCounts(rows: readonly AuditRow[]): Record<AuditCategory, number> {
	const out: Record<AuditCategory, number> = {
		permission: 0,
		dispatch: 0,
		access: 0,
		pairing: 0,
		people: 0,
	};
	for (const r of rows) out[r.category] = (out[r.category] ?? 0) + 1;
	return out;
}

export type AuditScope = 'personal' | 'project';

/**
 * D-05 `#scopeSw` on Audit (G-ACCESS §11.1): both scopes are live.
 * *project* sets `projectKey` to the active project; *personal* lists rows
 * where you are the actor or the subject — on T1 that is `who: me`; on T0
 * every row is the one owner's, so it is the whole log (§6.7).
 */
export function scopeFilter(
	scope: AuditScope,
	status: AccessStatus | null,
	projectId: string
): AuditFilter {
	if (!status) return {};
	const me = status.principal.principalId;
	if (scope === 'project') return { projectKey: `${me}/${projectId}` };
	return status.tier === 't1' ? { who: me } : {};
}

/** What `access_audit_export` is sent: the scope plus the chips (§6.8). */
export function exportFilter(base: AuditFilter, chips: ChipFilter): AuditFilter {
	const f: AuditFilter = { ...base };
	if (chips.who !== 'all') f.who = chips.who;
	if (chips.device !== 'all') f.device = chips.device;
	if (chips.kind !== 'all') f.category = chips.kind;
	if (chips.q.trim()) f.q = chips.q.trim();
	return f;
}

/** D-05's default export name: `ikenga-audit-<YYYY-MM-DD>.jsonl`. */
export function exportFileName(now: number): string {
	const d = new Date(now);
	return `ikenga-audit-${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}.jsonl`;
}

/**
 * §6.7 / P-14: reading the log needs effective `settings` in your own
 * workspace — a `full` device or a session. Below that the tab says why.
 */
export function auditReadReason(status: AccessStatus | null): string | null {
	if (!status) return null;
	if (status.share) return 'The audit log is read in your own workspace, not in a shared project';
	if (status.caps.includes('settings')) return null;
	const tier = TIER_LABELS[status.credential.tier].label;
	const where = status.credential.via === 'device' ? `this device is ${tier}` : `you have ${tier}`;
	return `Needs settings — ${where}. Open the audit log from a Full device or the host.`;
}

/** Whether this window may reseal (§6.4: T0, the operator bearer — the desktop). */
export function canReseal(status: AccessStatus | null): boolean {
	return status?.tier === 't0' && status.credential.via === 'operator';
}

/** The degraded banner (§6.4), verbatim. */
export function brokenBanner(seq: number): string {
	return `The audit chain is broken at #${seq} — access changes are paused. An operator must reseal it.`;
}

/** The rule box (D-05 `#s-audit .rulebox`), verbatim. */
export const AUDIT_RULE =
	'Append-only. Rows are written by the host and never edited or deleted from here; export writes a copy, it does not clear the log.';
