// G-ACCESS typed client (access-schema §9.1; WP-74a).
//
// One wrapper per command. Each one sends the same `{cmd, args}` shape over
// either transport: Tauri `invoke` (the desktop proxies `access_*` to its
// local daemon, P-20) and the browser's `/api/rpc`. The daemon (T0) or the
// broker (T1) decides every access question; this module never relies on
// RPC_REQUIREMENTS for security — it only uses it to disable a control with
// a reason (§1.5).
//
// W4/W5 WPs (75, 76, 77) read this file and do not edit it (§10.2): the
// wrappers for their commands are here from the start.

import { getTransport } from '../transport';
import { type ArmClass, type Cap, CAPS, type Role, type Tier, TIER_LABELS } from './caps.gen';
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
	/** `=== (tier === 't1')` (§4.5.5). */
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
}

export interface PairRequest {
	pairingId: string;
	deviceName: string;
	platform: string | null;
	remoteAddr: string;
	askedAt: number;
	code: string;
	fingerprint: [string, string, string, string];
	state: 'awaiting_host' | 'burned';
}

// ── Shapes the W4/W5 commands return. The §9.1 table fixes their args;
// ── these result shapes follow the §8.2 columns, camelCased.

export type RoutingMode = 'any_approve' | 'this_device';
export interface RoutingPref {
	mode: RoutingMode;
	deviceId: string | null;
}

export type PermissionDecision = 'allow_once' | 'allow_always_project' | 'deny';

export type MemberRole = Exclude<Role, 'owner'>;
export interface MemberView {
	principalId: string;
	username: string;
	role: MemberRole;
	scope: 'project' | 'artifact';
	artifactPath: string | null;
	expiresAt: number | null;
	addedAt: number;
	lastActiveAt: number | null;
}
export interface InviteView {
	inviteId: string;
	role: MemberRole;
	scope: 'project' | 'artifact';
	artifactPath: string | null;
	mode: 'email' | 'link';
	inviteeLabel: string | null;
	issuedAt: number;
	expiresAt: number;
	allowNewAccount: boolean;
}
export interface MembersList {
	owner: { principalId: string; username: string };
	members: MemberView[];
	invites: InviteView[];
	counts: Record<string, number>;
}
export interface ShareView {
	projectKey: string;
	projectName: string;
	ownerUsername: string;
	role: MemberRole;
	scope: 'project' | 'artifact';
	artifactPath: string | null;
	expiresAt: number | null;
}
export type PolicyCell = 'allowed' | 'withheld' | 'never';
export interface PolicyMatrix {
	matrix: Record<Role, Record<Cap, PolicyCell>>;
	ownerApprovalRequired: boolean;
}
export interface InviteIssue {
	projectId: string;
	mode: 'email' | 'link';
	inviteeLabel?: string;
	role: MemberRole;
	scope: 'project' | 'artifact';
	artifactPath?: string;
	memberExpiresAt?: number;
}
export interface IssuedInvite {
	inviteId: string;
	url: string;
	expiresAt: number;
	allowNewAccount: boolean;
}
export type AuditCategory = 'permission' | 'dispatch' | 'access' | 'pairing' | 'people';
export interface AuditFilter {
	who?: string;
	device?: string;
	category?: AuditCategory;
	q?: string;
	projectKey?: string;
}
export interface AuditRow {
	seq: number;
	atMs: number;
	kind: string;
	category: AuditCategory;
	principalId: string | null;
	deviceId: string | null;
	via: Via | 'cli' | 'system';
	subjectPrincipalId: string | null;
	subjectDeviceId: string | null;
	projectKey: string | null;
	target: string | null;
	remoteAddr: string | null;
	userAgent: string | null;
	detail: Record<string, unknown>;
	prevHash: string;
	hash: string;
}
export type LocalAuditKind =
	| 'app.locked'
	| 'app.unlocked'
	| 'vault.locked'
	| 'vault.unlocked'
	| 'permission.decided'
	| 'permission.refused';

/** The closed §9.1 error-code set. */
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

/** `<code>: <message>` → its code, or `null` for any other error. */
export function accessErrorCode(err: unknown): AccessErrorCode | null {
	const text = err instanceof Error ? err.message : typeof err === 'string' ? err : '';
	const code = text.split(':', 1)[0]?.trim();
	return (ACCESS_ERROR_CODES as readonly string[]).includes(code)
		? (code as AccessErrorCode)
		: null;
}

function call<T>(cmd: string, args: Record<string, unknown> = {}): Promise<T> {
	return getTransport().invoke<T>(cmd, args);
}

// ── Status, devices (WP-74) ──────────────────────────────────────────────
export const accessStatus = () => call<AccessStatus>('access_status');
export const accessDevicesList = () => call<DeviceView[]>('access_devices_list');
export const accessDeviceSetTier = (deviceId: string, tier: Tier) =>
	call<DeviceView>('access_device_set_tier', { deviceId, tier });
export const accessDeviceRevoke = (deviceId: string) =>
	call<Record<string, never>>('access_device_revoke', { deviceId });

// ── Pairing (WP-74b) ─────────────────────────────────────────────────────
export const accessPairBegin = (publicBase?: string) =>
	call<PairTicket>('access_pair_begin', publicBase ? { publicBase } : {});
export const accessPairCancel = (pairingId: string) =>
	call<Record<string, never>>('access_pair_cancel', { pairingId });
export const accessPairPending = () => call<PairRequest[]>('access_pair_pending');
export const accessPairDecide = (
	pairingId: string,
	decision: 'allow' | 'deny',
	tier?: Exclude<Tier, 'full'>
) =>
	call<{ device?: DeviceView }>('access_pair_decide', {
		pairingId,
		decision,
		...(tier ? { tier } : {}),
	});

// ── Routing and decisions (WP-75) ────────────────────────────────────────
export const accessRoutingGet = () => call<RoutingPref>('access_routing_get');
export const accessRoutingSet = (mode: RoutingMode, deviceId?: string) =>
	call<RoutingPref>('access_routing_set', deviceId ? { mode, deviceId } : { mode });
export const permissionDecide = (notificationId: number, decision: PermissionDecision) =>
	call<{ resolved: true }>('permission_decide', { notificationId, decision });

// ── Members, policies, invites, shares (WP-76) ──────────────────────────
export const accessMembersList = (projectId: string) =>
	call<MembersList>('access_members_list', { projectId });
export const accessMemberSetRole = (args: {
	projectId: string;
	principalId: string;
	role: MemberRole;
	artifactPath?: string;
	expiresAt?: number;
}) => call<MemberView>('access_member_set_role', args);
export const accessMemberRemove = (projectId: string, principalId: string) =>
	call<Record<string, never>>('access_member_remove', { projectId, principalId });
export const accessMemberRestore = (projectId: string, principalId: string) =>
	call<MemberView>('access_member_restore', { projectId, principalId });
export const accessPolicyGet = (projectId: string) =>
	call<PolicyMatrix>('access_policy_get', { projectId });
export const accessPolicySetCell = (
	projectId: string,
	role: MemberRole,
	cap: Exclude<Cap, 'secrets'>,
	allowed: boolean
) => call<PolicyMatrix>('access_policy_set_cell', { projectId, role, cap, allowed });
export const accessPolicySetOwnerApproval = (projectId: string, required: boolean) =>
	call<Record<string, never>>('access_policy_set_owner_approval', { projectId, required });
export const accessInviteIssue = (args: InviteIssue) =>
	call<IssuedInvite>('access_invite_issue', { ...args });
export const accessInviteRevoke = (inviteId: string) =>
	call<Record<string, never>>('access_invite_revoke', { inviteId });
export const accessSharesList = () => call<ShareView[]>('access_shares_list');

// ── Audit (WP-77) ────────────────────────────────────────────────────────
export const accessAuditList = (filter: AuditFilter = {}, before?: number, limit?: number) =>
	call<{ rows: AuditRow[]; nextBefore: number | null }>('access_audit_list', {
		filter,
		...(before !== undefined ? { before } : {}),
		...(limit !== undefined ? { limit } : {}),
	});
export const accessAuditVerify = () =>
	call<{ ok: boolean; rows: number; headHash: string; brokenAtSeq?: number }>(
		'access_audit_verify'
	);
/** `destPath` is honoured only for the T0 operator bearer (§6.8). */
export const accessAuditExport = (filter: AuditFilter = {}, destPath?: string) =>
	call<{ path: string } | { jsonl: string; truncated: boolean }>(
		'access_audit_export',
		destPath ? { filter, destPath } : { filter }
	);
export const accessAuditRecordLocal = (
	kind: LocalAuditKind,
	target: string,
	detail?: Record<string, unknown>
) =>
	call<Record<string, never>>('access_audit_record_local', {
		kind,
		target,
		...(detail ? { detail } : {}),
	});
export const accessAuditReseal = (ackSeq: number) =>
	call<Record<string, never>>('access_audit_reseal', { ackSeq });

// ── Disabling controls with a reason (§1.5) ──────────────────────────────

/** What `cmd` needs (§1.6); an unmapped command needs everything, owner-class. */
export function requirementOf(cmd: string): { caps: readonly Cap[]; class: ArmClass } {
	return RPC_REQUIREMENTS[cmd] ?? { caps: CAPS, class: 'owner' };
}

/** The caps of `cmd` the caller lacks, in canonical order. */
export function missingCaps(cmd: string, held: readonly Cap[]): Cap[] {
	return requirementOf(cmd).caps.filter((c) => !held.includes(c));
}

/**
 * Why a control for `cmd` is disabled for this caller, or `null` when it is
 * allowed. UI copy only: the server decides.
 * e.g. "Needs approve — this device is View + dispatch".
 */
export function disabledReason(cmd: string, status: AccessStatus): string | null {
	const req = requirementOf(cmd);
	if (req.class === 'operator' && status.credential.via !== 'operator') {
		return 'Only this computer can do that';
	}
	if (req.class === 'internal') return 'Not available here';
	if (req.class === 'owner' && status.share) {
		return 'Not available in a shared project';
	}
	const missing = missingCaps(cmd, status.caps);
	if (missing.length === 0) return null;
	const what = status.credential.via === 'device' ? 'this device' : 'this session';
	return `Needs ${missing.join(' + ')} — ${what} is ${TIER_LABELS[status.credential.tier].label}`;
}
