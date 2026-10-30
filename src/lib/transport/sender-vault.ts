// WP-K — the browser client for setting the box sender's credentials.
//
// The values go to the SENDER's vault only (never an account vault a pkg or
// Chi can read). The server relays to the sender socket as the signed-in
// principal; nothing here names a person or a role, and no reply ever carries
// a value. Mirrors `src-tauri/src/server/broker/vault.rs`.
//
// Deliberately NOT routed through `invoke()` (like server-update): a refusal
// must stay in the card, not open the re-auth dialog. The desktop never asks.

import { isTauri } from './index';
import { currentPrincipal, isT1Session } from './t1-session';

export type VaultOp = 'status' | 'set' | 'test' | 'clear';

export interface VaultKeyStatus {
	name: string;
	required: boolean;
}

export interface VaultGroupStatus {
	/** `outreach:<PERSON>` or `ruby_mail | buffer | listmonk | cms_draft | tasks`. */
	group: string;
	label: string | null;
	shared: boolean;
	names: VaultKeyStatus[];
	/** NAME -> value length in characters; only names that are set. */
	present: Record<string, number>;
	missing: string[];
	complete: boolean;
	lastChanged: string | null;
	lastChangedBy: string | null;
	readiness: { identityKnown?: boolean | null; approverListed?: boolean | null } | null;
}

export interface VaultStatus {
	person: string | null;
	admin: boolean;
	groups: VaultGroupStatus[];
}

export type VaultTested = 'login' | 'probe' | 'shape_only';

export interface VaultWriteData {
	group: string | null;
	names?: string[];
	replaced?: boolean;
	test?: string;
	tested?: VaultTested;
}

export type VaultResult<T> =
	| { ok: true; data: T }
	| {
			ok: false;
			/** The server's stable code (`test_failed`, `not_permitted`, `rate_limited`, ...), or `network`. */
			code: string;
			message: string;
			/** With `test_failed`: the fixed login-test code (`smtp_auth_failed`, ...). */
			test?: string;
			tested?: string;
			retryAfterSec?: number;
	  };

/** Whether this tab is a browser tab against the box (T1). The desktop and a
 *  T0 server never show the Sender section. */
export function mayManageSenderVault(): boolean {
	return !isTauri() && isT1Session();
}

/** Whether the signed-in principal is an account admin (first-layer hint; the
 *  sender decides). */
export function isSenderVaultAdmin(): boolean {
	return currentPrincipal()?.is_admin === true;
}

interface Body {
	ok?: boolean;
	data?: unknown;
	error?: unknown;
	code?: unknown;
	test?: unknown;
	tested?: unknown;
	retryAfterSec?: unknown;
}

export async function callSenderVault<T>(
	op: VaultOp,
	body: { group?: string; values?: Record<string, string> } = {}
): Promise<VaultResult<T>> {
	if (!mayManageSenderVault()) {
		return { ok: false, code: 'unavailable', message: 'Only available on the box.' };
	}
	let res: Response;
	try {
		res = await fetch(`/api/sender/vault/${op}`, {
			method: 'POST',
			headers: { 'Content-Type': 'application/json' },
			credentials: 'same-origin',
			body: JSON.stringify(body),
		});
	} catch {
		return { ok: false, code: 'network', message: 'Could not reach the server.' };
	}
	let parsed: Body | null = null;
	try {
		parsed = (await res.json()) as Body | null;
	} catch {
		parsed = null;
	}
	if (res.ok && parsed?.ok) return { ok: true, data: parsed.data as T };
	const str = (v: unknown) => (typeof v === 'string' ? v : undefined);
	return {
		ok: false,
		code: str(parsed?.code) ?? `http_${res.status}`,
		message: str(parsed?.error) ?? `Request failed (HTTP ${res.status}).`,
		test: str(parsed?.test),
		tested: str(parsed?.tested),
		retryAfterSec: typeof parsed?.retryAfterSec === 'number' ? parsed.retryAfterSec : undefined,
	};
}

export function fetchSenderVaultStatus(): Promise<VaultResult<VaultStatus>> {
	return callSenderVault<VaultStatus>('status');
}
