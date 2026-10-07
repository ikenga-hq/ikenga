// WP-40b — maps a `NotificationRow.action` (WP-40's producer-authored JSON,
// `{ kind, ...params }`, see `src-tauri/src/notifications/producers.rs` and
// `NotificationAction` in `src/lib/tauri-cmd.ts`) to the button(s) a
// popover row renders. Toasts (`components/ui/floating-toast-chip.tsx`) are
// text-only transient copies and carry no action. The action is narrowed at
// runtime with WP-40's `asKnownNotificationAction` (an ACP
// `permission.decide` becomes `open.thread`), and a hooks-gate
// Allow / Deny is offered only while the ask is live (`isPermissionAskLive`).
// WP-75 (G-ACCESS §5.5): every decision goes through `permission_decide`
// (hook and ACP asks alike), and an ask routed to another device (§5.1)
// offers no Allow / Deny here (`hostDecideBlock`).
//
// Deep-link fidelity notes (best-effort — no dedicated routes exist yet for
// some targets; see the WP-40b PR body):
//   - `open.terminal` / `open.thread` open a terminal pane keyed by the
//     given session/thread id, the same `addTab({ kind: 'terminal',
//     sessionId })` call the Explorer's Sessions section already uses
//     (`src/shell/explorer/sections/sessions.tsx`) — there is no separate
//     "open a chat thread" pane kind in this tree yet.
//   - `open.chi_run` has no run-detail route (WP-42 owns `/automations`'s
//     real run-history view and hasn't landed); it falls back to
//     `/automations?view=runs`, the same redirect target `/agent-runs`
//     already resolves to.
//   - `open.release_notes` falls back to `/settings/about` (WP-41's
//     release-notes sheet hasn't landed).
//   - `invite` ships no producer yet (D-05's people surface doesn't exist),
//     so its button is a guess: `/settings/people`.

import {
	accessRoutingGet,
	accessStatus,
	type PermissionDecision,
	parseAccessError,
	permissionDecide,
} from '@/lib/access/client';
import { iykeFetch } from '@/lib/iyke/client';
import { decideHookGateRemote } from '@/lib/iyke/terminal-hooks';
import { asKnownNotificationAction } from '@/lib/notifications/action-kind';
import { usePaneStore } from '@/lib/panes/pane-store';
import { type NotificationRow, notificationsList } from '@/lib/tauri-cmd';
import { isRemoteWebSession } from '@/lib/transport';

// Duplicated from `src/shell/status-bar.tsx`'s `NGWA_LINKS` rather than
// imported: this module is reached from the popover, and `status-bar.tsx`
// pulls in the bell that pulls in the popover — importing `NGWA_LINKS` here
// would close that loop into a real import cycle. Two route strings, kept in
// sync by hand.
const NGWA_UPDATES_ROUTE = '/packages?filter=updates';
const NGWA_VIOLATIONS_ROUTE = '/packages?filter=review';

export interface NotificationActionButton {
	label: string;
	variant: 'primary' | 'ghost';
	run: () => void;
}

function navigate(path: string): void {
	usePaneStore.getState().navigateFocused(path);
}

function openTerminalPane(sessionId: string): void {
	const { focusedId, addTab } = usePaneStore.getState();
	addTab(focusedId, { kind: 'terminal', sessionId });
}

/** What a decision that reached no live ask reads as (review WP78a-R5). */
export const ASK_ALREADY_OVER = 'This ask is already over';

/**
 * The pre-WP-75 path for the held hooks gate — the fallback when the decide
 * core can't take the row (an older backend, or the row not recorded yet).
 * Checked (review WP75-R10): resolves to `null` when the gate took the
 * decision, else the refusal message (or {@link ASK_ALREADY_OVER} when no
 * held gate took it), so a refused decision never looks answered. A `routing_refused` reply re-reads the host's routing and names
 * where the ask is answered (§5.7).
 */
export async function postHookDecision(
	requestId: string,
	decision: 'approved' | 'denied'
): Promise<string | null> {
	// A browser has no iyke bridge: a daemon terminal's gate is answered through
	// the daemon's own arm, which is `approve`-gated (so the routing preference
	// applies) and single-use.
	if (isRemoteWebSession()) {
		try {
			return (await decideHookGateRemote(requestId, decision)) ? null : ASK_ALREADY_OVER;
		} catch (e) {
			const { code, message } = parseAccessError(e);
			if (code === 'routing_refused') return (await refreshHostDecideBlock()) ?? message;
			return message || 'The decision was refused';
		}
	}
	let res: Response;
	try {
		res = await iykeFetch('/iyke/hooks/decision', {
			method: 'POST',
			headers: { 'Content-Type': 'application/json' },
			body: JSON.stringify({ requestId, decision }),
		});
	} catch (e) {
		return `Couldn't reach the permission gate: ${e instanceof Error ? e.message : String(e)}`;
	}
	if (res.ok) {
		// A 2xx with `gated: false` took nothing: the hold was already over
		// (answered, timed out as deny) — never presented as answered
		// (review WP78a-R5). No body / no flag: an older backend, answered.
		try {
			const body = (await res.json()) as { gated?: unknown };
			if (body?.gated === false) return ASK_ALREADY_OVER;
		} catch {
			// No JSON body.
		}
		return null;
	}
	let raw = '';
	try {
		const body = (await res.json()) as { error?: unknown };
		if (typeof body?.error === 'string') raw = body.error;
	} catch {
		// Not JSON: the status line below says what happened.
	}
	const { code, message } = parseAccessError(raw || `HTTP ${res.status}`);
	if (code === 'routing_refused') return (await refreshHostDecideBlock()) ?? message;
	return message || `The decision was refused (HTTP ${res.status})`;
}

/** The open `permission` row a held gate was recorded as
 *  (`permission:hook:<requestId>`), or `null`. */
export async function hookRowId(requestId: string): Promise<number | null> {
	try {
		const rows = await notificationsList({ kinds: ['permission'], limit: 200 });
		const key = `permission:hook:${requestId}`;
		return rows.find((r) => r.dedupeKey === key && r.resolvedAt == null)?.id ?? null;
	} catch {
		return null;
	}
}

/**
 * Decide a held hooks-gate ask by its request id (the Companion's permission
 * cards, the home "Waiting on you" tile): through `permission_decide` when
 * its row is recorded (§5.5), else the checked hooks route. Resolves to the
 * refusal message, or `null` when decided.
 */
export async function decideHookRequest(
	requestId: string,
	decision: 'approved' | 'denied'
): Promise<string | null> {
	const rowId = await hookRowId(requestId);
	if (rowId != null) {
		return decidePermissionRow(rowId, decision === 'denied' ? 'deny' : 'allow_once', requestId);
	}
	return postHookDecision(requestId, decision);
}

// ── G-ACCESS §5.5 / §5.7 (WP-75): one decide core ──────────────────────────

/**
 * Refusals the decide core made on purpose (§9.1 codes). Never worked around
 * by the hooks fallback: a `routing_refused` ask stays for the device it is
 * routed to (§5.1), a timed-out one stays denied (§5.6).
 */
const FINAL_DECIDE_CODES = new Set([
	'routing_refused',
	'owner_approval_required',
	'forbidden',
	'conflict',
	'answer_in_terminal',
	'invalid_request',
]);

/**
 * Decide a `permission` row through `permission_decide` (served in-process
 * on the desktop, §5.5): hook and ACP asks alike, capped by the routing
 * preference, attributed and audited. `hookRequestId` enables the
 * `/iyke/hooks/decision` fallback for a hooks-gate row the core could not
 * take. Resolves to the refusal message, or `null` when decided.
 */
export async function decidePermissionRow(
	rowId: number,
	decision: PermissionDecision,
	hookRequestId?: string
): Promise<string | null> {
	try {
		await permissionDecide(rowId, decision);
		return null;
	} catch (e) {
		const { code, message } = parseAccessError(e);
		if (code === 'routing_refused') void refreshHostDecideBlock();
		if (hookRequestId && (code === null || !FINAL_DECIDE_CODES.has(code))) {
			return postHookDecision(hookRequestId, decision === 'deny' ? 'denied' : 'approved');
		}
		return message;
	}
}

let hostBlock: string | null = null;

/**
 * Why THIS desktop may not answer its own asks right now (`waiting_on:
 * 'device'`, §5.7), or `null`. Set by {@link refreshHostDecideBlock}; read by
 * {@link notificationActionButtons} so a routed-away ask offers no dead
 * Allow / Deny.
 */
export function hostDecideBlock(): string | null {
	return hostBlock;
}

export function setHostDecideBlock(reason: string | null): void {
	hostBlock = reason;
}

/**
 * Re-read the host's routing (§5.1): the operator's effective caps already
 * apply the preference, so a store-backed status without `approve` means the
 * asks are answered on another device. No store → no preference → unblocked.
 */
export async function refreshHostDecideBlock(): Promise<string | null> {
	try {
		const status = await accessStatus();
		if (!status || status.store === 'none' || status.caps.includes('approve')) {
			hostBlock = null;
			return hostBlock;
		}
		const routing = (await accessRoutingGet()) as { deviceName?: string | null };
		hostBlock = routing.deviceName
			? `Answer on ${routing.deviceName} (this device only)`
			: 'Answered on the device chosen for asks (this device only)';
	} catch {
		// Unknown: leave the last answer; the daemon decides either way.
	}
	return hostBlock;
}

/** How long a Claude Code ACP round-trip waits (`PERMISSION_TIMEOUT_SECS`,
 *  `engines/claude_code/server.rs`) plus the same slack as the hooks gate. */
export const ACP_ASK_ANSWERABLE_MS = 310_000;

/**
 * How long a held hooks-gate ask can possibly still be answerable, from its
 * row's `createdAt`: the backend parks the `PreToolUse` response for
 * `GATE_HOLD_SECS` (30 s, `src-tauri/src/iyke/hook_settings.rs`) and curl
 * gives up at 35 s. Past this the gate has answered itself (timed out as
 * deny) even if the row never got its `resolvedAt` — e.g. the app quit
 * mid-hold. A little slack for clock skew between the DB write and render.
 */
export const HOOK_GATE_ANSWERABLE_MS = 40_000;

/**
 * Whether a hooks-gate `permission.decide` row can still take Allow / Deny.
 * `resolvedAt` is authoritative (WP-40 sets it when the human decides, the
 * gate times out, or the ask is otherwise over). A row from an older backend
 * without the field falls back to its read state — the gate marks its row
 * read when the ask ends. Either way an ask older than the hold window has
 * expired.
 */
export function isPermissionAskLive(row: NotificationRow, now: number = Date.now()): boolean {
	if (row.resolvedAt != null) return false;
	if (row.resolvedAt === undefined && row.readAt != null) return false;
	return now - row.createdAt < HOOK_GATE_ANSWERABLE_MS;
}

/**
 * Why this row's live ask offers no Allow / Deny here (§5.7: "Answer on
 * ned-desktop (this device only)"), or `null`. Only a still-answerable
 * permission ask — a held hooks gate or an ACP round-trip — is routed away;
 * an expired or resolved one is just over.
 */
export function notificationBlockedReason(
	row: NotificationRow,
	now: number = Date.now(),
	block: string | null = hostDecideBlock()
): string | null {
	if (!block || row.kind !== 'permission') return null;
	const action = asKnownNotificationAction(row.action);
	if (action?.kind === 'permission.decide') return isPermissionAskLive(row, now) ? block : null;
	if (action?.kind === 'open.thread') {
		return row.resolvedAt == null && now - row.createdAt < ACP_ASK_ANSWERABLE_MS ? block : null;
	}
	return null;
}

export function notificationActionButtons(
	row: NotificationRow,
	now: number = Date.now(),
	block: string | null = hostDecideBlock()
): NotificationActionButton[] {
	const action = asKnownNotificationAction(row.action);
	if (!action) {
		return row.kind === 'invite'
			? [{ label: 'Open People', variant: 'ghost', run: () => navigate('/settings/people') }]
			: [];
	}

	switch (action.kind) {
		case 'permission.decide': {
			// Resolved or expired: the decision is over, never offer a dead
			// Allow / Deny. Fall back to opening the terminal that asked.
			if (!isPermissionAskLive(row, now)) {
				const terminalId = action.terminalId;
				return terminalId
					? [{ label: 'Open terminal', variant: 'ghost', run: () => openTerminalPane(terminalId) }]
					: [];
			}
			const { requestId } = action;
			// Routed to another device (§5.1): no dead Allow / Deny here.
			if (block) {
				const terminalId = action.terminalId;
				return terminalId
					? [{ label: 'Open terminal', variant: 'ghost', run: () => openTerminalPane(terminalId) }]
					: [];
			}
			return [
				{
					label: 'Allow once',
					variant: 'primary',
					run: () => void decidePermissionRow(row.id, 'allow_once', requestId),
				},
				{
					label: 'Deny',
					variant: 'ghost',
					run: () => void decidePermissionRow(row.id, 'deny', requestId),
				},
			];
		}
		case 'open.terminal': {
			const sessionId = action.terminalId ?? action.sessionId;
			return sessionId
				? [{ label: 'Open terminal', variant: 'ghost', run: () => openTerminalPane(sessionId) }]
				: [];
		}
		case 'open.thread': {
			// An ACP ask: the thread's own dialog answers it, and since WP-75
			// so does `permission_decide` (§5.5) while the round-trip waits.
			const { threadId } = action;
			const open: NotificationActionButton[] = threadId
				? [{ label: 'Open thread', variant: 'ghost', run: () => openTerminalPane(threadId) }]
				: [];
			const live =
				row.kind === 'permission' &&
				row.resolvedAt == null &&
				now - row.createdAt < ACP_ASK_ANSWERABLE_MS;
			if (!live || block) return open;
			return [
				{
					label: 'Allow once',
					variant: 'primary',
					run: () => void decidePermissionRow(row.id, 'allow_once'),
				},
				{
					label: 'Always for this project',
					variant: 'ghost',
					run: () => void decidePermissionRow(row.id, 'allow_always_project'),
				},
				{ label: 'Deny', variant: 'ghost', run: () => void decidePermissionRow(row.id, 'deny') },
				...open,
			];
		}
		case 'open.chi_run': {
			const label = action.status === 'failed' ? 'Open log' : 'Open artifact';
			return [{ label, variant: 'ghost', run: () => navigate('/automations?view=runs') }];
		}
		case 'open.release_notes':
			return [{ label: 'Release notes', variant: 'ghost', run: () => navigate('/settings/about') }];
		case 'open.pkg_updates':
			return [{ label: 'View update', variant: 'ghost', run: () => navigate(NGWA_UPDATES_ROUTE) }];
		case 'open.violations':
			return [{ label: 'Review', variant: 'primary', run: () => navigate(NGWA_VIOLATIONS_ROUTE) }];
		default:
			return row.kind === 'invite'
				? [{ label: 'Open People', variant: 'ghost', run: () => navigate('/settings/people') }]
				: [];
	}
}
