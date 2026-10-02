// "Shared with you" and share mode — G-ACCESS §4.5.2, WP-76 (P-28).
//
// - `SharedWithYou`: the Members tab block listing the projects other people
//   shared with this principal (`access_shares_list`), each with **Open**.
//   Open switches the transport into share mode (every RPC carries
//   `X-Ikenga-Share`, every WebSocket `?share=`) and lands on the project.
// - `ShareModeBanner`: while a share is open, "royalti-co · shared by ada ·
//   Reviewer" with **Leave shared project**. Mounted through `ReauthOverlay`,
//   the session chrome every browser boot root already renders.
//
// No Explorer change in v1 (P-28): the shared project is the only project a
// share request can see, so the app simply shows it.

import { ExternalLink, LogOut, Users } from 'lucide-react';
import { useEffect, useState } from 'react';

import { Button } from '@/components/ui/button';
import { accessSharesList, parseAccessError } from '@/lib/access/client';
import {
	currentShare,
	onShareModeChange,
	type ShareSelection,
	setShareMode,
} from '@/lib/transport';

import { Kv, PeopleBlock } from './frame';

/** `access_shares_list`'s `ShareView` (§9.1). */
export interface ShareView {
	projectKey: string;
	ownerPrincipalId: string;
	ownerUsername: string | null;
	projectId: string;
	projectName: string;
	role: 'operator' | 'reviewer' | 'guest';
	scope: 'project' | 'artifact';
	artifactPath: string | null;
	expiresAt: number | null;
	addedAt: number;
}

const ROLE_LABEL: Record<ShareView['role'], string> = {
	operator: 'Operator',
	reviewer: 'Reviewer',
	guest: 'Guest',
};

/** The transport's selection for a share. */
export function selectionOf(v: ShareView): ShareSelection {
	return {
		projectKey: v.projectKey,
		projectId: v.projectId,
		projectName: v.projectName,
		ownerUsername: v.ownerUsername,
		role: v.role,
		scope: v.scope,
		artifactPath: v.artifactPath,
	};
}

/** "royalti-co · shared by ada · Reviewer" (§4.5.2). */
export function bannerLine(s: ShareSelection): string {
	const by = s.ownerUsername ? ` · shared by ${s.ownerUsername}` : '';
	const what = s.scope === 'artifact' && s.artifactPath ? ` · ${s.artifactPath}` : '';
	return `${s.projectName}${by} · ${ROLE_LABEL[s.role]}${what}`;
}

/** Open a share: select it, then land on the project (a full reload, so
 *  every cache and socket starts over inside the share). */
export function openShare(v: ShareView | ShareSelection): void {
	const sel = 'ownerPrincipalId' in v ? selectionOf(v) : v;
	setShareMode(sel);
	if (typeof window !== 'undefined') window.location.assign('/');
}

/** Leave share mode and go back to your own workspace. */
export function leaveShare(): void {
	setShareMode(null);
	if (typeof window !== 'undefined') window.location.assign('/settings/members');
}

export function SharedWithYou() {
	const [shares, setShares] = useState<ShareView[] | null>(null);
	const [error, setError] = useState<string | null>(null);
	useEffect(() => {
		let cancelled = false;
		accessSharesList()
			.then((v) => {
				if (!cancelled) setShares(v as ShareView[]);
			})
			.catch((e) => {
				if (!cancelled) setError(parseAccessError(e).message);
			});
		return () => {
			cancelled = true;
		};
	}, []);
	if (!error && (shares === null || shares.length === 0)) return null;
	return (
		<PeopleBlock title="Shared with you" right={<Kv>{shares?.length ?? 0}</Kv>}>
			<div data-state="shared-with-you" className="divide-y divide-[var(--border-soft)]">
				{(shares ?? []).map((s) => (
					<div key={s.projectKey} className="flex flex-wrap items-center gap-3 py-2">
						<Users className="h-3.5 w-3.5 text-[var(--fg-muted)]" />
						<span className="min-w-0 flex-1">
							<span className="block font-medium text-[var(--text-caption,12px)] text-[var(--fg)]">
								{s.projectName}
							</span>
							<Kv>
								shared by {s.ownerUsername ?? 'someone'} · {ROLE_LABEL[s.role]}
								{s.scope === 'artifact' && s.artifactPath ? ` · ${s.artifactPath}` : ''}
							</Kv>
						</span>
						<Button type="button" size="xs" variant="outline" onClick={() => openShare(s)}>
							<ExternalLink /> Open
						</Button>
					</div>
				))}
				{error && (
					<p role="alert" className="m-0 py-2 text-[12px] text-[var(--danger)]">
						{error}
					</p>
				)}
			</div>
		</PeopleBlock>
	);
}

/** The share-mode strip (§4.5.2). Renders nothing outside a share. */
export function ShareModeBanner() {
	const [share, setShare] = useState<ShareSelection | null>(() => currentShare());
	useEffect(() => onShareModeChange(setShare), []);
	if (!share) return null;
	return (
		<div
			data-state="share-mode"
			role="status"
			className="pointer-events-none fixed inset-x-0 top-0 z-40 flex justify-center"
		>
			<div className="pointer-events-auto mt-1 flex items-center gap-2 rounded-md border border-[var(--border)] bg-[var(--bg-surface)] px-3 py-1 text-[var(--text-micro)] text-[var(--fg)] shadow-md">
				<Users className="h-3 w-3 text-[var(--fg-muted)]" />
				<span>{bannerLine(share)}</span>
				<Button type="button" size="xs" variant="ghost" onClick={leaveShare}>
					<LogOut /> Leave shared project
				</Button>
			</div>
		</div>
	);
}
