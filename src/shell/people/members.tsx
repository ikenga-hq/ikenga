// Members tab — D-05 `members` / `members-shared` (`designs/people.html`),
// WP-76 (G-ACCESS §4, §4.5.2, §7).
//
// - T0 (the desktop, or a T0 daemon): one principal. D-05's solo state ("Just
//   you."), Share kola disabled with "Sharing needs an Ikenga server with
//   accounts (T1)", and the rule box kept as drawn (§4.5.5).
// - T1: the project's people (`access_members_list`): the Owner's synthetic
//   row (role fixed — "A project keeps one Owner"), each member's role menu
//   (§4.2: to Guest needs one artifact and an expiry; away from Guest resets
//   both), scope, added, last active, Remove with a 10 s Undo
//   (`access_member_restore`, P-17); pending invites with Revoke (no Undo,
//   D-6) or Dismiss once expired (§7.4); "Shared with you" (§4.5.2).
// - In a share, only the Owner and Operators see the list; a Reviewer or
//   Guest sees why.
//
// The access arms decide; this view only disables controls with a reason.

import { MoreHorizontal, Plus, RefreshCw, Undo2 } from 'lucide-react';
import { useCallback, useEffect, useMemo, useState } from 'react';

import { Button } from '@/components/ui/button';
import {
	Dialog,
	DialogContent,
	DialogDescription,
	DialogFooter,
	DialogHeader,
	DialogTitle,
} from '@/components/ui/dialog';
import {
	DropdownMenu,
	DropdownMenuContent,
	DropdownMenuItem,
	DropdownMenuRadioGroup,
	DropdownMenuRadioItem,
	DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu';
import { FloatingToastChip } from '@/components/ui/floating-toast-chip';
import { StatusChip } from '@/components/ui/status-chip';
import type { Role } from '@/lib/access/caps.gen';
import {
	type AccessStatus,
	accessInviteRevoke,
	accessMemberRemove,
	accessMemberRestore,
	accessMemberSetRole,
	accessMembersList,
	accessStatus,
	parseAccessError,
} from '@/lib/access/client';
import { useShellStore } from '@/lib/shell/shell-store';
import { currentShare } from '@/lib/transport';
import { isT1Session } from '@/lib/transport/t1-session';

import { relativeTime } from './devices-model';
import { Kv, PeopleBlock, PeopleFileBar, PeopleHeader } from './frame';
import { ShareKolaSheet } from './share-kola-sheet';
import { SharedWithYou } from './shared-with-you';

// ── model ────────────────────────────────────────────────────────────────────

export type MemberRole = Exclude<Role, 'owner'>;

/** `access_members_list`'s `MemberView` (§9.1). */
export interface MemberView {
	principalId: string;
	username: string | null;
	role: MemberRole;
	scope: 'project' | 'artifact';
	artifactPath: string | null;
	expiresAt: number | null;
	addedAt: number;
	lastActiveAt: number | null;
	weeklySpendCapCents: number | null;
}

/** `access_members_list`'s `InviteView` (§9.1). */
export interface InviteView {
	inviteId: string;
	label: string | null;
	mode: 'email' | 'link';
	role: MemberRole;
	scope: 'project' | 'artifact';
	artifactPath: string | null;
	issuedAt: number;
	issuedBy: string;
	expiresAt: number;
	memberExpiresAt: number | null;
	allowNewAccount: boolean;
	state: 'pending' | 'expired';
}

export interface MembersList {
	projectKey: string;
	projectName: string | null;
	owner: { principalId: string; username: string | null };
	counts: { members: number; pendingInvites: number };
	members: MemberView[];
	invites: InviteView[];
}

export type AccessTier = 't0' | 't1';

/** The tier this window runs against: a T1 session, else T0 (the desktop,
 *  a T0 daemon). `access_status` refines it when it answers. */
export function tierOf(status: AccessStatus | null): AccessTier {
	if (status) return status.tier;
	return isT1Session() ? 't1' : 't0';
}

export const ROLE_TITLES: Readonly<Record<Role, string>> = {
	owner: 'Owner',
	operator: 'Operator',
	reviewer: 'Reviewer',
	guest: 'Guest',
};

/** D-05 role menu copy. */
export const ROLE_CHOICES: ReadonlyArray<{ id: MemberRole; label: string; sub: string }> = [
	{ id: 'operator', label: 'Operator', sub: 'Can dispatch and approve' },
	{ id: 'reviewer', label: 'Reviewer', sub: 'Read and comment; cannot dispatch' },
	{ id: 'guest', label: 'Guest', sub: 'One artifact, for a while' },
];

export const SOLO_DISABLED_REASON = 'Sharing needs an Ikenga server with accounts (T1)';
export const OWNER_FIXED_REASON = 'A project keeps one Owner. Transfer it from the ⋯ menu.';
export const TRANSFER_DISABLED_REASON =
	"Transfer needs moving the project to the new owner's workspace — coming later";

/** "expires in 4 d" / "expired" (D-05 scope column). */
export function expiryLabel(expiresAt: number | null, now: number): string | null {
	if (expiresAt === null) return null;
	const ms = expiresAt - now;
	if (ms <= 0) return 'expired';
	const mins = Math.round(ms / 60_000);
	if (mins < 60) return `expires in ${Math.max(1, mins)} min`;
	const hours = Math.round(mins / 60);
	if (hours < 48) return `expires in ${hours} h`;
	return `expires in ${Math.round(hours / 24)} d`;
}

/** A short day stamp: "12 Sep". */
export function dayStamp(ms: number): string {
	return new Date(ms).toLocaleDateString(undefined, { day: 'numeric', month: 'short' });
}

/** Which Members state renders (D-05 `members` vs `members-shared`). */
export function membersMode(tier: AccessTier, list: MembersList | null): 'solo' | 'shared' {
	if (tier === 't0' || !list) return 'solo';
	return list.members.length > 0 || list.invites.length > 0 ? 'shared' : 'solo';
}

/** The person cell's line. */
export function personName(m: Pick<MemberView, 'username' | 'principalId'>): string {
	return m.username ?? `${m.principalId.slice(0, 8)}…`;
}

// ── hooks ────────────────────────────────────────────────────────────────────

/** `access_status` once per mount; `null` while loading or where the store is
 *  unavailable (the desktop without its daemon: T0, solo). */
export function useAccessStatus(): {
	status: AccessStatus | null;
	tier: AccessTier;
	loaded: boolean;
} {
	const [status, setStatus] = useState<AccessStatus | null>(null);
	const [loaded, setLoaded] = useState(false);
	useEffect(() => {
		let cancelled = false;
		accessStatus()
			.then((s) => {
				if (!cancelled) setStatus(s);
			})
			.catch(() => {
				if (!cancelled) setStatus(null);
			})
			.finally(() => {
				if (!cancelled) setLoaded(true);
			});
		return () => {
			cancelled = true;
		};
	}, []);
	return { status, tier: tierOf(status), loaded };
}

/** The project the Members and Policies tabs are about: the share's in share
 *  mode, else the active project. */
export function useTabProject(): { projectId: string; projectName: string } {
	const activeProjectId = useShellStore((s) => s.activeProjectId);
	const projects = useShellStore((s) => s.projects);
	const share = currentShare();
	if (share) return { projectId: share.projectId, projectName: share.projectName };
	const id = activeProjectId || 'default';
	const p = projects.find((x) => x.id === id);
	return { projectId: id, projectName: p?.display_name ?? id };
}

export function useMembersList(projectId: string, enabled: boolean) {
	const [list, setList] = useState<MembersList | null>(null);
	const [error, setError] = useState<string | null>(null);
	const [loading, setLoading] = useState(false);
	const reload = useCallback(async () => {
		if (!enabled) return;
		setLoading(true);
		try {
			setList((await accessMembersList(projectId)) as MembersList);
			setError(null);
		} catch (e) {
			const { code, message } = parseAccessError(e);
			setList(null);
			setError(code === 'requires_t1' ? null : message);
		} finally {
			setLoading(false);
		}
	}, [projectId, enabled]);
	useEffect(() => {
		void reload();
	}, [reload]);
	return { list, error, loading, reload };
}

// ── the tab ──────────────────────────────────────────────────────────────────

export function MembersTab() {
	const { status, tier, loaded } = useAccessStatus();
	const { projectId, projectName } = useTabProject();
	const share = currentShare();
	const canList = tier === 't1' && (!share || share.role === 'operator');
	const { list, error, loading, reload } = useMembersList(projectId, loaded && canList);
	const [sharing, setSharing] = useState(false);
	const mode = membersMode(tier, list);
	const name = list?.projectName ?? projectName;
	const shareDisabled =
		tier === 't0'
			? SOLO_DISABLED_REASON
			: status && !status.adminStrength
				? 'Needs a password session or a Full device'
				: share && share.role !== 'operator'
					? 'Only the Owner and Operators can invite'
					: null;

	return (
		<div
			data-state={mode === 'shared' ? 'members-shared' : 'members'}
			className="mx-auto w-full max-w-[960px] space-y-4 px-6 py-6"
		>
			<PeopleHeader tab="members" />
			{tier === 't1' && share && !canList ? (
				<PeopleBlock title={`People on ${name}`}>
					<p className="m-0 py-3 text-[var(--text-caption,12px)] text-[var(--fg-muted)]">
						You're a {ROLE_TITLES[share.role]} here. Only the Owner and Operators see who else has
						access.
					</p>
				</PeopleBlock>
			) : mode === 'solo' ? (
				<SoloBlock
					name={name}
					disabledReason={shareDisabled}
					onShare={() => setSharing(true)}
					error={error}
				/>
			) : (
				list && (
					<>
						<PeopleTable
							list={list}
							name={name}
							projectId={projectId}
							canEdit={!share && status?.adminStrength === true}
							shareDisabled={shareDisabled}
							onShare={() => setSharing(true)}
							onChanged={() => void reload()}
							loading={loading}
						/>
						<PendingInvites
							list={list}
							canRevoke={status?.adminStrength === true}
							onChanged={() => void reload()}
						/>
					</>
				)
			)}
			<RuleBox tier={tier} />
			{tier === 't1' && !share && <SharedWithYou />}
			<ShareKolaSheet
				open={sharing}
				onOpenChange={setSharing}
				projectId={projectId}
				projectName={name}
				onIssued={() => void reload()}
			/>
			<PeopleFileBar t1={tier === 't1'} />
		</div>
	);
}

function SoloBlock({
	name,
	disabledReason,
	onShare,
	error,
}: {
	name: string;
	disabledReason: string | null;
	onShare: () => void;
	error: string | null;
}) {
	return (
		<PeopleBlock title={`People on ${name}`}>
			<div className="flex flex-col items-center gap-3 py-8 text-center">
				<h3
					className="m-0 text-xl font-semibold text-[var(--fg)]"
					style={{ fontFamily: 'var(--font-display)' }}
				>
					Just you.
				</h3>
				<p className="m-0 max-w-[340px] text-[var(--text-caption,12px)] leading-relaxed text-[var(--fg-muted)]">
					That is the normal setup, and nothing in Ikenga is waiting for a second person. Share kola
					when you actually want someone looking at this project — not before.
				</p>
				<span title={disabledReason ?? undefined}>
					<Button
						type="button"
						size="sm"
						onClick={onShare}
						disabled={disabledReason !== null}
						aria-describedby={disabledReason ? 'share-kola-why' : undefined}
					>
						<Plus /> Share kola
					</Button>
				</span>
				{disabledReason && (
					<Kv>
						<span id="share-kola-why">{disabledReason}</span>
					</Kv>
				)}
				<Kv>
					Roles and what each may do live in{' '}
					<a href="/settings/policies" className="font-semibold text-[var(--fg)]">
						Policies
					</a>
					.
				</Kv>
				{error && (
					<p role="alert" className="m-0 text-[12px] text-[var(--danger)]">
						{error}
					</p>
				)}
			</div>
		</PeopleBlock>
	);
}

/** D-05's dashed rule box under the table. */
function RuleBox({ tier }: { tier: AccessTier }) {
	return (
		<p className="m-0 rounded-[var(--radius-md,6px)] border border-dashed border-[var(--border)] px-3 py-2.5 text-[var(--text-caption,12px)] text-[var(--fg-muted)]">
			{tier === 't0' ? (
				'Sharing needs an account, because the other person needs something to sign in to. Everything else on this screen works signed out.'
			) : (
				<>
					<b className="font-semibold text-[var(--fg)]">
						Secrets are not on this list and never will be.
					</b>{' '}
					The vault is on the host; a shared project shares files, sessions and dispatch, not
					credentials.
				</>
			)}
		</p>
	);
}

function PeopleTable({
	list,
	name,
	projectId,
	canEdit,
	shareDisabled,
	onShare,
	onChanged,
	loading,
}: {
	list: MembersList;
	name: string;
	projectId: string;
	canEdit: boolean;
	shareDisabled: string | null;
	onShare: () => void;
	onChanged: () => void;
	loading: boolean;
}) {
	const now = Date.now();
	const [rowError, setRowError] = useState<string | null>(null);
	const [removing, setRemoving] = useState<MemberView | null>(null);
	const [undo, setUndo] = useState<MemberView | null>(null);
	const [guestFor, setGuestFor] = useState<MemberView | null>(null);

	const setRole = async (
		m: MemberView,
		role: MemberRole,
		extra?: { artifactPath: string; expiresAt: number }
	) => {
		setRowError(null);
		try {
			await accessMemberSetRole({ projectId, principalId: m.principalId, role, ...extra });
			onChanged();
		} catch (e) {
			setRowError(parseAccessError(e).message);
		}
	};
	const restore = async (m: MemberView) => {
		setUndo(null);
		try {
			await accessMemberRestore(projectId, m.principalId);
			onChanged();
		} catch (e) {
			setRowError(parseAccessError(e).message);
		}
	};

	return (
		<PeopleBlock
			title={`People on ${name}`}
			right={
				<>
					{loading && <RefreshCw className="h-3 w-3 animate-spin" aria-label="Loading" />}
					<span title={shareDisabled ?? undefined}>
						<Button
							type="button"
							variant="outline"
							size="xs"
							onClick={onShare}
							disabled={shareDisabled !== null}
						>
							<Plus /> Share kola
						</Button>
					</span>
				</>
			}
		>
			<div className="-mx-3 overflow-x-auto">
				<table className="w-full border-collapse text-left text-[var(--text-caption,12px)]">
					<thead>
						<tr className="text-[var(--text-micro)] uppercase tracking-[0.08em] text-[var(--fg-muted)]">
							<th className="px-3 py-1.5 font-semibold">Person</th>
							<th className="px-3 py-1.5 font-semibold">Role</th>
							<th className="px-3 py-1.5 font-semibold">Scope</th>
							<th className="px-3 py-1.5 font-semibold">Added</th>
							<th className="px-3 py-1.5 font-semibold">Last active</th>
							<th className="px-3 py-1.5" />
						</tr>
					</thead>
					<tbody>
						<tr className="border-t border-[var(--border-soft)]" data-member="owner">
							<td className="px-3 py-2">
								<span className="block font-medium text-[var(--fg)]">
									{list.owner.username ?? 'Owner'}
								</span>
								{/* D-05 `m.self`: the Owner's own row reads "you · this device". */}
								<Kv>{currentShare() ? 'Owner' : 'you · this device'}</Kv>
							</td>
							<td className="px-3 py-2">
								<span title={OWNER_FIXED_REASON}>
									<Button type="button" variant="outline" size="xs" disabled>
										Owner
									</Button>
								</span>
							</td>
							<td className="px-3 py-2">Project</td>
							<td className="px-3 py-2 text-[var(--fg-muted)]">—</td>
							<td className="px-3 py-2 text-[var(--fg-muted)]">—</td>
							<td className="px-3 py-2 text-right">
								<DropdownMenu>
									<DropdownMenuTrigger asChild>
										<Button type="button" variant="ghost" size="xs" aria-label="Owner actions">
											<MoreHorizontal />
										</Button>
									</DropdownMenuTrigger>
									<DropdownMenuContent align="end">
										<DropdownMenuItem disabled title={TRANSFER_DISABLED_REASON}>
											Transfer ownership…
										</DropdownMenuItem>
										<DropdownMenuItem disabled className="text-[11px]">
											{TRANSFER_DISABLED_REASON}
										</DropdownMenuItem>
									</DropdownMenuContent>
								</DropdownMenu>
							</td>
						</tr>
						{list.members.map((m) => (
							<tr
								key={m.principalId}
								className="border-t border-[var(--border-soft)]"
								data-member={m.role}
							>
								<td className="px-3 py-2 font-mono text-[var(--fg)]">{personName(m)}</td>
								<td className="px-3 py-2">
									<RoleMenu
										member={m}
										disabled={!canEdit}
										onPick={(r) => (r === 'guest' ? setGuestFor(m) : void setRole(m, r))}
									/>
								</td>
								<td className="px-3 py-2">
									{m.scope === 'artifact' ? (
										<span className="font-mono text-[var(--text-micro)]">{m.artifactPath}</span>
									) : (
										'Project'
									)}
									{m.expiresAt !== null && (
										<span className="block text-[var(--text-micro)] text-[var(--fg-muted)]">
											{expiryLabel(m.expiresAt, now)}
										</span>
									)}
								</td>
								<td className="px-3 py-2 text-[var(--fg-muted)]">{dayStamp(m.addedAt)}</td>
								<td className="px-3 py-2 text-[var(--fg-muted)]">
									{m.lastActiveAt ? relativeTime(m.lastActiveAt, now) : '—'}
								</td>
								<td className="px-3 py-2 text-right">
									<Button
										type="button"
										variant="outline"
										size="xs"
										className="text-[var(--danger)]"
										disabled={!canEdit}
										title={canEdit ? undefined : 'Only the Owner can remove people'}
										onClick={() => setRemoving(m)}
									>
										Remove
									</Button>
								</td>
							</tr>
						))}
					</tbody>
				</table>
			</div>
			{rowError && (
				<p role="alert" className="m-0 py-2 text-[12px] text-[var(--danger)]">
					{rowError}
				</p>
			)}
			<RemoveConfirm
				member={removing}
				projectId={projectId}
				onClose={() => setRemoving(null)}
				onRemoved={(m) => {
					setUndo(m);
					onChanged();
				}}
			/>
			<GuestScopeDialog
				member={guestFor}
				onClose={() => setGuestFor(null)}
				onPick={(artifactPath, expiresAt) => {
					const m = guestFor;
					setGuestFor(null);
					if (m) void setRole(m, 'guest', { artifactPath, expiresAt });
				}}
			/>
			{undo && (
				<div data-member-toast="removed">
					<FloatingToastChip
						variant="info"
						anchor="viewport-bottom-right"
						label={`Removed ${personName(undo)}`}
						action={{
							label: 'Undo',
							icon: <Undo2 className="h-3 w-3" />,
							onClick: () => void restore(undo),
						}}
						ttlMs={10_000}
						onDismiss={() => setUndo(null)}
					/>
				</div>
			)}
		</PeopleBlock>
	);
}

function RoleMenu({
	member,
	disabled,
	onPick,
}: {
	member: MemberView;
	disabled: boolean;
	onPick: (r: MemberRole) => void;
}) {
	if (disabled) {
		return <StatusChip tone="muted">{ROLE_TITLES[member.role]}</StatusChip>;
	}
	return (
		<DropdownMenu>
			<DropdownMenuTrigger asChild>
				<Button
					type="button"
					variant="outline"
					size="xs"
					aria-label={`${personName(member)}'s role`}
				>
					{ROLE_TITLES[member.role]}
				</Button>
			</DropdownMenuTrigger>
			<DropdownMenuContent align="start" className="w-[260px]">
				<DropdownMenuRadioGroup
					value={member.role}
					onValueChange={(v) => v !== member.role && onPick(v as MemberRole)}
				>
					{ROLE_CHOICES.map((r) => (
						<DropdownMenuRadioItem key={r.id} value={r.id} className="flex-col items-start">
							<span>{r.label}</span>
							<span className="text-[11px] text-[var(--fg-muted)]">{r.sub}</span>
						</DropdownMenuRadioItem>
					))}
				</DropdownMenuRadioGroup>
			</DropdownMenuContent>
		</DropdownMenu>
	);
}

/** §4.2: a Guest gets one artifact and an expiry; "Never" isn't offered. */
function GuestScopeDialog({
	member,
	onClose,
	onPick,
}: {
	member: MemberView | null;
	onClose: () => void;
	onPick: (artifactPath: string, expiresAt: number) => void;
}) {
	const [path, setPath] = useState('');
	const [days, setDays] = useState(7);
	useEffect(() => {
		setPath(member?.artifactPath ?? '');
		setDays(7);
	}, [member]);
	const valid = path.trim().length > 0 && !path.trim().startsWith('/') && !path.includes('..');
	return (
		<Dialog open={member !== null} onOpenChange={(o) => !o && onClose()}>
			<DialogContent data-state="members-guest" className="bg-[var(--bg-surface)] text-[var(--fg)]">
				<DialogHeader>
					<DialogTitle>Make {member ? personName(member) : ''} a Guest</DialogTitle>
					<DialogDescription>
						A Guest sees one artifact, for a while. Pick the file (relative to the project) and when
						their access ends.
					</DialogDescription>
				</DialogHeader>
				<label className="flex flex-col gap-1 text-[var(--text-micro)] text-[var(--fg-muted)]">
					Artifact
					<input
						value={path}
						onChange={(e) => setPath(e.target.value)}
						placeholder="plans/shell/board.html"
						className="h-7 rounded-[var(--radius-sm)] border border-[var(--border)] bg-[var(--bg-sunken)] px-2 font-mono text-[12px] text-[var(--fg)] outline-none focus:border-[var(--primary)]"
					/>
				</label>
				<label className="flex flex-col gap-1 text-[var(--text-micro)] text-[var(--fg-muted)]">
					Expires
					<select
						value={days}
						onChange={(e) => setDays(Number(e.target.value))}
						className="h-7 rounded-[var(--radius-sm)] border border-[var(--border)] bg-[var(--bg-sunken)] px-2 text-[12px] text-[var(--fg)]"
					>
						<option value={1}>In 1 day</option>
						<option value={7}>In 7 days</option>
						<option value={30}>In 30 days</option>
					</select>
				</label>
				<DialogFooter>
					<Button type="button" variant="outline" size="sm" onClick={onClose}>
						Cancel
					</Button>
					<Button
						type="button"
						size="sm"
						disabled={!valid}
						onClick={() => onPick(path.trim(), Date.now() + days * 86_400_000)}
					>
						Make Guest
					</Button>
				</DialogFooter>
			</DialogContent>
		</Dialog>
	);
}

function RemoveConfirm({
	member,
	projectId,
	onClose,
	onRemoved,
}: {
	member: MemberView | null;
	projectId: string;
	onClose: () => void;
	onRemoved: (m: MemberView) => void;
}) {
	const [busy, setBusy] = useState(false);
	const [error, setError] = useState<string | null>(null);
	const remove = async () => {
		if (!member) return;
		setBusy(true);
		setError(null);
		try {
			await accessMemberRemove(projectId, member.principalId);
			onRemoved(member);
			onClose();
		} catch (e) {
			setError(parseAccessError(e).message);
		} finally {
			setBusy(false);
		}
	};
	return (
		<Dialog open={member !== null} onOpenChange={(o) => !o && onClose()}>
			<DialogContent
				data-state="members-remove"
				className="bg-[var(--bg-surface)] text-[var(--fg)]"
			>
				<DialogHeader>
					<DialogTitle>Remove {member ? personName(member) : ''}?</DialogTitle>
					<DialogDescription>
						They lose access to this project at once and their open connections drop. You can undo
						for 10 seconds.
					</DialogDescription>
				</DialogHeader>
				{error && (
					<p role="alert" className="m-0 text-[12px] text-[var(--danger)]">
						{error}
					</p>
				)}
				<DialogFooter>
					<Button type="button" variant="outline" size="sm" onClick={onClose}>
						Keep them
					</Button>
					<Button
						type="button"
						variant="destructive"
						size="sm"
						disabled={busy}
						onClick={() => void remove()}
					>
						Remove
					</Button>
				</DialogFooter>
			</DialogContent>
		</Dialog>
	);
}

function PendingInvites({
	list,
	canRevoke,
	onChanged,
}: {
	list: MembersList;
	canRevoke: boolean;
	onChanged: () => void;
}) {
	const now = Date.now();
	const [error, setError] = useState<string | null>(null);
	const pending = useMemo(() => list.invites.filter((i) => i.state === 'pending').length, [list]);
	if (list.invites.length === 0) return null;
	const revoke = async (i: InviteView) => {
		setError(null);
		try {
			await accessInviteRevoke(i.inviteId);
			onChanged();
		} catch (e) {
			setError(parseAccessError(e).message);
		}
	};
	return (
		<PeopleBlock title="Pending invites" right={<Kv>{pending}</Kv>}>
			<div className="-mx-3 overflow-x-auto">
				<table className="w-full border-collapse text-left text-[var(--text-caption,12px)]">
					<thead>
						<tr className="text-[var(--text-micro)] uppercase tracking-[0.08em] text-[var(--fg-muted)]">
							<th className="px-3 py-1.5 font-semibold">Invited</th>
							<th className="px-3 py-1.5 font-semibold">Role</th>
							<th className="px-3 py-1.5 font-semibold">Scope</th>
							<th className="px-3 py-1.5 font-semibold">Sent</th>
							<th className="px-3 py-1.5" />
						</tr>
					</thead>
					<tbody>
						{list.invites.map((i) => (
							<tr
								key={i.inviteId}
								className="border-t border-[var(--border-soft)]"
								data-invite={i.state}
							>
								<td className="px-3 py-2 font-mono text-[var(--fg)]">
									{i.label ?? 'Link invite'}
									{i.state === 'expired' && (
										<span className="ml-2">
											<StatusChip tone="warn">expired</StatusChip>
										</span>
									)}
								</td>
								<td className="px-3 py-2">{ROLE_TITLES[i.role]}</td>
								<td className="px-3 py-2">
									{i.scope === 'artifact' ? (
										<span className="font-mono text-[var(--text-micro)]">{i.artifactPath}</span>
									) : (
										'Project'
									)}
								</td>
								<td className="px-3 py-2 text-[var(--fg-muted)]">
									{relativeTime(i.issuedAt, now)}
								</td>
								<td className="px-3 py-2 text-right">
									<Button
										type="button"
										variant="outline"
										size="xs"
										className={i.state === 'pending' ? 'text-[var(--danger)]' : undefined}
										disabled={!canRevoke}
										onClick={() => void revoke(i)}
									>
										{i.state === 'pending' ? 'Revoke invite' : 'Dismiss'}
									</Button>
								</td>
							</tr>
						))}
					</tbody>
				</table>
			</div>
			{error && (
				<p role="alert" className="m-0 py-2 text-[12px] text-[var(--danger)]">
					{error}
				</p>
			)}
		</PeopleBlock>
	);
}
