// Share kola — D-05 `share-kola` (`designs/people.html`), WP-76
// (G-ACCESS §7.2).
//
// How (Email | Link), Who, Role, Scope, Expires, and "What they will be able
// to see" rendered from the role's row — §4.1's defaults with the project's
// overrides (`access_policy_get`) — never from mockup copy. The invite fixes
// role, scope and artifact at issue (Locked).
//
// Deviations forced by Locked decisions (§11.2):
// - D-9: "Create invite", not "Send invite": no email is sent in v1 (N-4).
//   Both modes show the link with Copy link; email mode records the address.
// - D-16: an invite that can't create an account (`allowNewAccount=false`,
//   N-11) says "They need an account on this server — ask an admin to
//   create one."
// - D-10: no footer iyke line (N-5).

import { Check, Copy, Minus, ShieldAlert } from 'lucide-react';
import { type ReactNode, useEffect, useId, useMemo, useState } from 'react';

import { Button } from '@/components/ui/button';
import {
	Dialog,
	DialogContent,
	DialogDescription,
	DialogFooter,
	DialogHeader,
	DialogTitle,
} from '@/components/ui/dialog';
import { FloatingToastChip } from '@/components/ui/floating-toast-chip';
import { cn } from '@/components/ui/utils';
import { type Cap, ROLE_DEFAULT_CAPS } from '@/lib/access/caps.gen';
import { accessInviteIssue, accessPolicyGet, parseAccessError } from '@/lib/access/client';

import { NewChip } from './devices-pair-confirm';
import { D05_FOCUS } from './focus';
import type { MemberRole } from './members';

export type InviteMode = 'email' | 'link';
export type InviteScope = 'project' | 'artifact';

/** D-05 expiry choices; `null` = Never (not offered to a Guest, §4.2). */
export const EXPIRY_CHOICES: ReadonlyArray<{ days: number | null; label: string }> = [
	{ days: null, label: 'Never' },
	{ days: 1, label: 'In 1 day' },
	{ days: 7, label: 'In 7 days' },
	{ days: 30, label: 'In 30 days' },
];

/** The copy D-16 adds when an invite can't create an account (N-11). */
export const NEEDS_ACCOUNT_COPY =
	'They need an account on this server — ask an admin to create one.';

export interface SeeItem {
	id: string;
	label: string;
	sub: string | null;
	allowed: boolean;
}

/**
 * D-05 "What they will be able to see", from the role's caps (`row`: the
 * matrix row, overrides applied) and the scope. An artifact scope grants
 * `files` read of exactly that path (§4.2). Writes need `files` + `dispatch`
 * (P-2). Secrets: never (§4.1).
 */
export function seeList(role: MemberRole, scope: InviteScope, row: readonly Cap[]): SeeItem[] {
	const has = (c: Cap) => row.includes(c);
	const artifact = scope === 'artifact';
	const files = artifact || has('files');
	return [
		{
			id: 'files',
			label: artifact ? 'One artifact' : 'Files in this project',
			sub: !files
				? null
				: artifact
					? 'Read-only, that one file and its comments.'
					: has('dispatch')
						? 'Read and write.'
						: 'Read-only.',
			allowed: files,
		},
		{
			id: 'sessions',
			label: 'Sessions',
			sub: has('sessions')
				? role === 'reviewer'
					? 'Read-only transcripts. No cost figures.'
					: 'Transcripts, tool feed and cost.'
				: null,
			allowed: has('sessions') && !artifact,
		},
		{
			id: 'dispatch',
			label: 'Dispatch to Chi',
			sub: has('dispatch')
				? 'They can instruct Chi in this project.'
				: 'They can comment, not instruct.',
			allowed: has('dispatch') && !artifact,
		},
		{
			id: 'approve',
			label: 'Approve permissions',
			sub: has('approve') ? 'Asks that touch secrets still go to you.' : null,
			allowed: has('approve') && !artifact,
		},
		{
			id: 'settings',
			label: 'Settings and packages',
			sub: null,
			allowed: (has('settings') || has('install')) && !artifact,
		},
		{ id: 'secrets', label: 'Secrets', sub: 'Never.', allowed: false },
	];
}

/** The invite's member expiry (unix ms) for an expiry choice. */
export function memberExpiry(days: number | null, now: number): number | undefined {
	return days === null ? undefined : now + days * 86_400_000;
}

/** Why "Create invite" is disabled, or `null`. */
export function issueBlocker(f: {
	mode: InviteMode;
	who: string;
	role: MemberRole;
	scope: InviteScope;
	artifactPath: string;
	days: number | null;
}): string | null {
	if (f.mode === 'email' && !/^[^\s@]+@[^\s@]+$/.test(f.who.trim())) {
		return 'Enter their email address';
	}
	if (f.scope === 'artifact' || f.role === 'guest') {
		const p = f.artifactPath.trim();
		if (!p) return 'Name the artifact (a path inside the project)';
		if (p.startsWith('/') || p.split('/').includes('..') || p.includes('\\')) {
			return 'The artifact path is relative to the project, without ..';
		}
	}
	if (f.role === 'guest' && f.days === null) return 'A Guest’s access must expire';
	return null;
}

/** The role's row in this project: `access_policy_get`, else §4.1's default. */
function useRoleRows(projectId: string, open: boolean) {
	const [rows, setRows] = useState<Record<MemberRole, Cap[]> | null>(null);
	useEffect(() => {
		if (!open) return;
		let cancelled = false;
		accessPolicyGet(projectId)
			.then((p) => {
				if (cancelled) return;
				const row = (r: MemberRole) =>
					(Object.entries(p.matrix[r]) as [Cap, string][])
						.filter(([, v]) => v === 'allowed')
						.map(([c]) => c);
				setRows({ operator: row('operator'), reviewer: row('reviewer'), guest: row('guest') });
			})
			.catch(() => {
				if (!cancelled) setRows(null);
			});
		return () => {
			cancelled = true;
		};
	}, [projectId, open]);
	return (r: MemberRole): Cap[] => rows?.[r] ?? [...ROLE_DEFAULT_CAPS[r]];
}

export function ShareKolaSheet({
	open,
	onOpenChange,
	projectId,
	projectName,
	onIssued,
}: {
	open: boolean;
	onOpenChange: (o: boolean) => void;
	projectId: string;
	projectName: string;
	onIssued: () => void;
}) {
	const [mode, setMode] = useState<InviteMode>('email');
	const [who, setWho] = useState('');
	const [role, setRole] = useState<MemberRole>('reviewer');
	const [scope, setScope] = useState<InviteScope>('project');
	const [artifactPath, setArtifactPath] = useState('');
	const [days, setDays] = useState<number | null>(7);
	const [busy, setBusy] = useState(false);
	const [error, setError] = useState<string | null>(null);
	const [issued, setIssued] = useState<{ url: string; allowNewAccount: boolean } | null>(null);
	const [copied, setCopied] = useState(false);
	const [toast, setToast] = useState<string | null>(null);
	const rowOf = useRoleRows(projectId, open);
	const ids = { who: useId(), role: useId(), scope: useId(), expires: useId(), path: useId() };

	useEffect(() => {
		if (!open) return;
		setIssued(null);
		setError(null);
		setCopied(false);
	}, [open]);
	// §4.2: a Guest is one artifact with an expiry; leaving Guest keeps the scope.
	useEffect(() => {
		if (role === 'guest') {
			setScope('artifact');
			if (days === null) setDays(7);
		}
	}, [role, days]);

	const effectiveScope: InviteScope = role === 'guest' ? 'artifact' : scope;
	const see = useMemo(
		() => seeList(role, effectiveScope, rowOf(role)),
		[role, effectiveScope, rowOf]
	);
	const blocker = issueBlocker({ mode, who, role, scope: effectiveScope, artifactPath, days });

	const create = async () => {
		setBusy(true);
		setError(null);
		try {
			const out = await accessInviteIssue({
				projectId,
				mode,
				...(who.trim() ? { inviteeLabel: who.trim() } : {}),
				role,
				scope: effectiveScope,
				...(effectiveScope === 'artifact' ? { artifactPath: artifactPath.trim() } : {}),
				...(days !== null ? { memberExpiresAt: memberExpiry(days, Date.now()) } : {}),
			});
			const url = new URL(out.url, window.location.origin).toString();
			setIssued({ url, allowNewAccount: out.allowNewAccount });
			setToast(
				mode === 'email'
					? `Invite created for ${who.trim()} — copy the link to send it.`
					: 'Invite link created — copy it to send it.'
			);
			onIssued();
		} catch (e) {
			setError(parseAccessError(e).message);
		} finally {
			setBusy(false);
		}
	};

	const copy = async () => {
		if (!issued) return;
		try {
			await navigator.clipboard.writeText(issued.url);
			setCopied(true);
		} catch {
			setCopied(false);
		}
	};

	const field =
		'h-8 rounded-[var(--radius-sm)] border border-[var(--border)] bg-[var(--bg-sunken)] px-2 text-[length:var(--text-caption,12px)] text-[var(--fg)] outline-none focus:border-[var(--primary)]';

	return (
		<>
			<Dialog open={open} onOpenChange={onOpenChange}>
				<DialogContent
					data-state="share-kola"
					className={`${D05_FOCUS} max-w-[760px] bg-[var(--bg-surface)] text-[var(--fg)] sm:max-w-[760px]`}
				>
					<DialogHeader>
						<DialogTitle className="flex items-center gap-2">
							<span style={{ fontFamily: 'var(--font-display)' }}>Share kola</span>
							<NewChip />
							<span className="font-mono text-[length:var(--text-micro)] font-normal text-[var(--fg-muted)]">
								{projectName}
							</span>
						</DialogTitle>
						<DialogDescription>
							An invite is single-use and expires. Role and scope are fixed when you create it.
						</DialogDescription>
					</DialogHeader>
					<div className="grid gap-5 md:grid-cols-[1fr_260px]">
						<div className="space-y-3">
							<Row label="How">
								<div className="inline-flex rounded-[var(--radius-sm)] border border-[var(--border)]">
									{(['email', 'link'] as const).map((m) => (
										<button
											key={m}
											type="button"
											aria-pressed={mode === m}
											onClick={() => setMode(m)}
											className={cn(
												'px-3 py-1 text-[length:var(--text-caption,12px)]',
												mode === m
													? 'bg-[var(--primary-soft)] font-semibold text-[var(--fg)]'
													: 'text-[var(--fg-muted)]'
											)}
										>
											{m === 'email' ? 'Email' : 'Link'}
										</button>
									))}
								</div>
							</Row>
							<Row label="Who" htmlFor={ids.who}>
								<input
									id={ids.who}
									type={mode === 'email' ? 'email' : 'text'}
									value={who}
									onChange={(e) => setWho(e.target.value)}
									placeholder={
										mode === 'email' ? 'name@example.com' : 'A note for the list (optional)'
									}
									className={cn(field, 'w-full max-w-[320px] font-mono')}
								/>
							</Row>
							<Row label="Role" htmlFor={ids.role}>
								<select
									id={ids.role}
									value={role}
									onChange={(e) => setRole(e.target.value as MemberRole)}
									className={field}
								>
									<option value="operator">Operator</option>
									<option value="reviewer">Reviewer</option>
									<option value="guest">Guest</option>
								</select>
							</Row>
							<Row label="Scope" htmlFor={ids.scope}>
								<select
									id={ids.scope}
									value={effectiveScope}
									disabled={role === 'guest'}
									onChange={(e) => setScope(e.target.value as InviteScope)}
									className={field}
								>
									<option value="project">Whole project</option>
									<option value="artifact">One artifact</option>
								</select>
								{effectiveScope === 'artifact' && (
									<input
										id={ids.path}
										aria-label="Artifact path"
										value={artifactPath}
										onChange={(e) => setArtifactPath(e.target.value)}
										placeholder="plans/shell/board.html"
										className={cn(field, 'w-full max-w-[320px] font-mono')}
									/>
								)}
							</Row>
							<Row label="Expires" htmlFor={ids.expires}>
								<select
									id={ids.expires}
									value={days === null ? 'never' : String(days)}
									onChange={(e) =>
										setDays(e.target.value === 'never' ? null : Number(e.target.value))
									}
									className={field}
								>
									{EXPIRY_CHOICES.map((c) => (
										<option
											key={c.label}
											value={c.days === null ? 'never' : String(c.days)}
											disabled={c.days === null && role === 'guest'}
										>
											{c.label}
										</option>
									))}
								</select>
							</Row>
						</div>
						<div>
							<h4 className="m-0 mb-2 text-[length:var(--text-micro)] font-semibold uppercase tracking-[0.1em] text-[var(--fg-muted)]">
								What they will be able to see
							</h4>
							<ul className="m-0 list-none space-y-2 p-0" data-see-list>
								{see.map((item) => (
									<li key={item.id} className="flex gap-2" data-allowed={item.allowed}>
										<span className="mt-0.5 flex-none">
											{item.id === 'secrets' ? (
												<ShieldAlert className="h-3.5 w-3.5 text-[var(--danger)]" />
											) : item.allowed ? (
												<Check className="h-3.5 w-3.5 text-[var(--live)]" />
											) : (
												<Minus className="h-3.5 w-3.5 text-[var(--fg-muted)]" />
											)}
										</span>
										<span>
											<span
												className={cn(
													'block text-[length:var(--text-caption,12px)]',
													item.allowed ? 'font-medium text-[var(--fg)]' : 'text-[var(--fg-muted)]'
												)}
											>
												{item.label}
											</span>
											{item.sub && (
												<span className="block text-[length:var(--text-micro)] text-[var(--fg-muted)]">
													{item.sub}
												</span>
											)}
										</span>
									</li>
								))}
							</ul>
						</div>
					</div>
					{issued && (
						<div
							data-state="share-kola-created"
							className="space-y-2 rounded-[var(--radius-md,6px)] border border-[var(--border)] bg-[var(--bg-sunken)] p-3"
						>
							<div className="flex items-center gap-2">
								<code className="min-w-0 flex-1 truncate font-mono text-[11px] text-[var(--fg)]">
									{issued.url}
								</code>
								<Button type="button" size="xs" variant="outline" onClick={() => void copy()}>
									<Copy /> {copied ? 'Copied' : 'Copy link'}
								</Button>
							</div>
							<p className="m-0 text-[length:var(--text-micro)] text-[var(--fg-muted)]">
								No email is sent — send them this link yourself. It works once.
							</p>
							{!issued.allowNewAccount && (
								<p
									className="m-0 text-[length:var(--text-micro)] text-[var(--fg)]"
									data-needs-account
								>
									{NEEDS_ACCOUNT_COPY}
								</p>
							)}
						</div>
					)}
					{error && (
						<p role="alert" className="m-0 text-[12px] text-[var(--danger)]">
							{error}
						</p>
					)}
					<DialogFooter>
						<Button type="button" variant="outline" size="sm" onClick={() => onOpenChange(false)}>
							{issued ? 'Done' : 'Cancel'}
						</Button>
						{!issued && (
							<span title={blocker ?? undefined}>
								<Button
									type="button"
									size="sm"
									disabled={busy || blocker !== null}
									onClick={() => void create()}
								>
									Create invite
								</Button>
							</span>
						)}
					</DialogFooter>
				</DialogContent>
			</Dialog>
			{toast && (
				<div data-invite-toast>
					<FloatingToastChip
						variant="notice"
						anchor="viewport-bottom-right"
						label={toast}
						ttlMs={5000}
						onDismiss={() => setToast(null)}
					/>
				</div>
			)}
		</>
	);
}

function Row({
	label,
	htmlFor,
	children,
}: {
	label: string;
	htmlFor?: string;
	children: ReactNode;
}) {
	return (
		<div className="flex min-h-8 flex-wrap items-center gap-3">
			<span className="w-[80px] flex-none text-[length:var(--text-caption,12px)] font-medium text-[var(--fg)]">
				{htmlFor ? <label htmlFor={htmlFor}>{label}</label> : label}
			</span>
			<span className="flex min-w-0 flex-1 flex-wrap items-center gap-2">{children}</span>
		</div>
	);
}
