// G-ACCESS §9.1 — typed wrappers for the access surface (WP-74a). The shapes
// are normative (plans/shell-ux-rearchitecture/drafts/access-schema.md §9.1).
//
// On the desktop each `access_*` command proxies to the local daemon, the
// access store's only opener (P-20); in a browser the same names go over
// `/api/rpc`. `permission_decide` is served in-process on the desktop (§5.5).
//
// `RPC_REQUIREMENTS` is used only to DISABLE controls with a reason
// ("Needs approve — this device is View + dispatch"); the daemon / broker
// decides (§1.5). Never treat these helpers as security.

import { invoke } from '@/lib/tauri-cmd';
import { type ArmClass, type Cap, type Role, TIER_LABELS, type Tier } from './caps.gen';
import { RPC_REQUIREMENTS } from './rpc-requirements.gen';

export type Via = 'session' | 'device' | 'operator';

export interface AccessStatus {
	tier: 't0' | 't1';
	store: 'ok' | 'degraded' | 'none';
	brokenAtSeq?: number;
	principal: { principalId: string; username: string; isAdmin: boolean };
	credential: { via: Via; deviceId: string | null; tier: Tier };
	caps: Cap[];
	adminStrength: boolean;
	publicUrl: string | null;
	/** `=== (tier === 't1')` (§4.5.5, Round 16). */
	sharingEnabled: boolean;
	share: null | {
		projectKey: string;
		projectName: string;
		ownerUsername: string;
		role: Role;
		scope: 'project' | 'artifact';
		artifactPath?: string;
	};
}

export interface DeviceView {
	deviceId: string;
	kind: 'host' | 'paired';
	name: string;
	platform: string | null;
	tier: Tier;
	pairedAt: number;
	lastSeenAt: number | null;
	lastSeenAddr: string | null;
	liveSockets: number;
	thisDevice: boolean;
}

export interface PairTicket {
	pairingId: string;
	/** `'K7P-42Q'` */
	code: string;
	expiresAt: number;
	pairUrl: string | null;
	qrPayload: string | null;
	/**
	 * The device cookie a device opening `pairUrl` gets carries `Secure`. A
	 * browser drops it over plain HTTP off loopback, so the sheet warns when
	 * `pairUrl` is `http://` and this is true (WP-74b review M1). False under
	 * `--insecure-cookie`, and on the T0 daemon when `pairUrl`'s host is a
	 * tailnet address (Round 19, DEC-R19-1). Absent on older daemons.
	 */
	cookieSecure?: boolean;
}

export interface PairRequest {
	pairingId: string;
	deviceName: string;
	platform: string | null;
	remoteAddr: string;
	askedAt: number;
	code: string;
	fingerprint: [string, string, string, string];
	/** `paused`: burned by the host-wide pause, which still lasts (§3.7). */
	state: 'awaiting_host' | 'burned' | 'paused';
	/** With `paused`: ms until pairing reopens. */
	retryAfterMs?: number | null;
}

export type RoutingMode = 'any_approve' | 'this_device';
export type PermissionDecision = 'allow_once' | 'allow_always_project' | 'deny';
export type AuditLocalKind =
	| 'app.locked'
	| 'app.unlocked'
	| 'vault.locked'
	| 'vault.unlocked'
	| 'permission.decided'
	| 'permission.refused';

/** The closed error-code set (§9.1). RPC errors read `<code>: <message>`. */
export const ACCESS_ERROR_CODES = [
	'unauthenticated',
	'forbidden',
	'not_found',
	'conflict',
	'gone',
	'expired',
	'throttled',
	'invalid_request',
	'routing_refused',
	'owner_approval_required',
	'answer_in_terminal',
	'requires_t1',
	'served_by_broker',
	'audit_unavailable',
	'store_unavailable',
	'internal',
] as const;
export type AccessErrorCode = (typeof ACCESS_ERROR_CODES)[number];

/** Split an access error string into its code and message. */
export function parseAccessError(err: unknown): { code: AccessErrorCode | null; message: string } {
	const text = err instanceof Error ? err.message : String(err);
	const idx = text.indexOf(':');
	const head = idx >= 0 ? text.slice(0, idx).trim() : '';
	const code = (ACCESS_ERROR_CODES as readonly string[]).includes(head)
		? (head as AccessErrorCode)
		: null;
	return { code, message: code ? text.slice(idx + 1).trim() : text };
}

/** What `cmd` needs (§1.6); an unmapped command is `{all 7, owner}` (rule 2). */
export function requirementFor(cmd: string): { caps: readonly Cap[]; class: ArmClass } {
	return (
		RPC_REQUIREMENTS[cmd] ?? {
			caps: ['files', 'sessions', 'dispatch', 'approve', 'install', 'settings', 'secrets'],
			class: 'owner',
		}
	);
}

/** The caps `cmd` needs that `caps` lacks. */
export function missingCaps(cmd: string, caps: readonly Cap[]): Cap[] {
	const have = new Set(caps);
	return requirementFor(cmd).caps.filter((c) => !have.has(c));
}

/**
 * A disabled-control reason for `cmd` under `status`, or `null` when the
 * control may be enabled. UI copy only — never a security decision.
 */
export function disabledReason(cmd: string, status: AccessStatus): string | null {
	const req = requirementFor(cmd);
	if (req.class === 'operator' && status.credential.via !== 'operator') {
		return 'Only the host can do this';
	}
	if (req.class === 'owner' && status.share) {
		return 'Not available in a shared project';
	}
	const missing = missingCaps(cmd, status.caps);
	if (missing.length === 0) return null;
	const tier = TIER_LABELS[status.credential.tier].label;
	const where = status.credential.via === 'device' ? `this device is ${tier}` : `you have ${tier}`;
	return `Needs ${missing.join(' + ')} — ${where}`;
}

// ── devices (WP-74) ──────────────────────────────────────────────────────────

export const accessStatus = () => invoke<AccessStatus>('access_status', {});
export const accessDevicesList = () => invoke<DeviceView[]>('access_devices_list', {});
export const accessDeviceSetTier = (deviceId: string, tier: Tier) =>
	invoke<DeviceView>('access_device_set_tier', { deviceId, tier });
export const accessDeviceRevoke = (deviceId: string) =>
	invoke<Record<string, never>>('access_device_revoke', { deviceId });
export const accessPairBegin = (publicBase?: string) =>
	invoke<PairTicket>('access_pair_begin', publicBase ? { publicBase } : {});
export const accessPairCancel = (pairingId: string) =>
	invoke<Record<string, never>>('access_pair_cancel', { pairingId });
export const accessPairPending = () => invoke<PairRequest[]>('access_pair_pending', {});
export const accessPairDecide = (
	pairingId: string,
	decision: 'allow' | 'deny',
	tier?: Exclude<Tier, 'full'>
) =>
	invoke<{ device?: DeviceView }>('access_pair_decide', {
		pairingId,
		decision,
		...(tier ? { tier } : {}),
	});

// ── permission routing (WP-75) ───────────────────────────────────────────────

export const accessRoutingGet = () =>
	invoke<{ mode: RoutingMode; deviceId: string | null }>('access_routing_get', {});
export const accessRoutingSet = (mode: RoutingMode, deviceId?: string) =>
	invoke<{ mode: RoutingMode; deviceId: string | null }>('access_routing_set', {
		mode,
		...(deviceId ? { deviceId } : {}),
	});
export const permissionDecide = (notificationId: number, decision: PermissionDecision) =>
	invoke<{ resolved: true }>('permission_decide', { notificationId, decision });

// ── members, policies, invites, shares (WP-76) ───────────────────────────────

export const accessMembersList = (projectId: string) =>
	invoke<unknown>('access_members_list', { projectId });
export const accessMemberSetRole = (args: {
	projectId: string;
	principalId: string;
	role: Exclude<Role, 'owner'>;
	artifactPath?: string;
	expiresAt?: number;
}) => invoke<unknown>('access_member_set_role', args);
export const accessMemberRemove = (projectId: string, principalId: string) =>
	invoke<Record<string, never>>('access_member_remove', { projectId, principalId });
export const accessMemberRestore = (projectId: string, principalId: string) =>
	invoke<unknown>('access_member_restore', { projectId, principalId });
export const accessPolicyGet = (projectId: string) =>
	invoke<{
		matrix: Record<Role, Record<Cap, 'allowed' | 'withheld' | 'never'>>;
		ownerApprovalRequired: boolean;
	}>('access_policy_get', { projectId });
export const accessPolicySetCell = (
	projectId: string,
	role: Exclude<Role, 'owner'>,
	cap: Exclude<Cap, 'secrets'>,
	allowed: boolean
) => invoke<unknown>('access_policy_set_cell', { projectId, role, cap, allowed });
export const accessPolicySetOwnerApproval = (projectId: string, required: boolean) =>
	invoke<Record<string, never>>('access_policy_set_owner_approval', { projectId, required });
export const accessInviteIssue = (args: {
	projectId: string;
	mode: 'email' | 'link';
	inviteeLabel?: string;
	role: Exclude<Role, 'owner'>;
	scope: 'project' | 'artifact';
	artifactPath?: string;
	memberExpiresAt?: number;
}) =>
	invoke<{ inviteId: string; url: string; expiresAt: number; allowNewAccount: boolean }>(
		'access_invite_issue',
		args
	);
export const accessInviteRevoke = (inviteId: string) =>
	invoke<Record<string, never>>('access_invite_revoke', { inviteId });
export const accessSharesList = () => invoke<unknown[]>('access_shares_list', {});

// ── audit (WP-77) ────────────────────────────────────────────────────────────

export interface AuditFilter {
	who?: string;
	device?: string;
	category?: 'permission' | 'dispatch' | 'access' | 'pairing' | 'people';
	q?: string;
	projectKey?: string;
}

export const accessAuditList = (filter: AuditFilter, before?: number, limit?: number) =>
	invoke<{ rows: unknown[]; nextBefore: number | null }>('access_audit_list', {
		filter,
		...(before !== undefined ? { before } : {}),
		...(limit !== undefined ? { limit } : {}),
	});
export const accessAuditVerify = () =>
	invoke<{ ok: boolean; rows: number; headHash: string; brokenAtSeq?: number }>(
		'access_audit_verify',
		{}
	);
export const accessAuditExport = (filter: AuditFilter, destPath?: string) =>
	invoke<{ path: string } | { jsonl: string; truncated: boolean }>('access_audit_export', {
		filter,
		...(destPath ? { destPath } : {}),
	});
export const accessAuditRecordLocal = (kind: AuditLocalKind, target: string, detail?: unknown) =>
	invoke<Record<string, never>>('access_audit_record_local', {
		kind,
		target,
		...(detail !== undefined ? { detail } : {}),
	});
export const accessAuditReseal = (ackSeq: number) =>
	invoke<Record<string, never>>('access_audit_reseal', { ackSeq });

// ── push (plans/pwa S2 §7) ───────────────────────────────────────────────────
//
// Browser-only: the desktop never registers a service worker, so these arms
// are daemon-only (`server/parity.rs`). Called from `src/lib/pwa/push-client.ts`,
// never from components directly.

/** Every kind a push can carry (W3). `test` is never selectable. */
export type PushKind =
	| 'permission'
	| 'run_finished'
	| 'run_failed'
	| 'run_cancelled'
	| 'invite'
	| 'pairing'
	| 'update';

export type PushConfig =
	| {
			enabled: true;
			/** `applicationServerKey`, base64url. */
			publicKey: string;
			keyId: string;
			/** The kinds this credential may receive. */
			kinds: PushKind[];
	  }
	| { enabled: false; reason?: string };

export interface PushSubscriptionView {
	subId: string;
	label: string | null;
	via: 'device' | 'session' | 'operator';
	deviceId: string | null;
	/** The push service's host only — never the endpoint path. */
	endpointHost: string;
	kinds: PushKind[];
	createdAt: number;
	lastSuccessAt: number | null;
	thisDevice: boolean;
}

export const accessPushConfig = () => invoke<PushConfig>('access_push_config', {});
export const accessPushSubscribe = (args: {
	endpoint: string;
	keys: { p256dh: string; auth: string };
	kinds?: PushKind[];
	label?: string;
}) => invoke<{ subId: string }>('access_push_subscribe', args);
export const accessPushUpdate = (subId: string, kinds: PushKind[]) =>
	invoke<{ kinds: PushKind[] }>('access_push_update', { subId, kinds });
export const accessPushUnsubscribe = (target: { subId: string } | { endpoint: string }) =>
	invoke<{ removed: number }>('access_push_unsubscribe', target);
export const accessPushList = () => invoke<PushSubscriptionView[]>('access_push_list', {});
export const accessPushTest = (subId: string) =>
	invoke<{ queued: true }>('access_push_test', { subId });
