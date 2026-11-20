// WP-17: the Links panel (Settings → People → Profile) — the outside links
// you made, with state, expiry, opens, downloads, last opened and Revoke; and,
// for an admin, "All links" with a filter by account, Revoke and Revoke all.
// Hidden unless the server turned file links on (`access_status.fileLinksEnabled`).
// A link's URL is never listed: only its hash is stored, so a lost link is
// replaced by making a new one.

import { RefreshCw } from 'lucide-react';
import { useCallback, useEffect, useState } from 'react';

import { Button } from '@/components/ui/button';
import {
	type AccessStatus,
	type AdminFileLinkView,
	accessFileLinkAdminRevoke,
	accessFileLinkRevoke,
	accessFileLinksAdminList,
	accessFileLinksList,
	accessStatus,
	type FileLinkView,
	parseAccessError,
} from '@/lib/access/client';

import {
	canRevoke,
	expiryText,
	filterByOwner,
	isLinksAdmin,
	LINK_STATE_LABEL,
	linksOffered,
	ownersOf,
} from './file-link-model';
import { D05_DANGER } from './focus';
import { Kv, PeopleBlock } from './frame';
import { relativeTime } from './devices-model';

export function FileLinksBlock() {
	const [status, setStatus] = useState<AccessStatus | null>(null);
	useEffect(() => {
		let live = true;
		accessStatus()
			.then((s) => live && setStatus(s))
			.catch(() => live && setStatus(null));
		return () => {
			live = false;
		};
	}, []);
	if (!linksOffered(status)) return null;
	return (
		<div data-state="file-links" className="space-y-4">
			<MyLinks />
			{isLinksAdmin(status) && <AllLinks />}
		</div>
	);
}

function LinkRows({
	links,
	onRevoke,
	owners,
	empty,
}: {
	links: readonly (FileLinkView | AdminFileLinkView)[];
	onRevoke: (linkId: string) => Promise<void>;
	owners?: boolean;
	empty: string;
}) {
	const [confirming, setConfirming] = useState<string | null>(null);
	const now = Date.now();
	if (links.length === 0) {
		return <p className="m-0 py-3 text-[length:var(--text-caption,12px)] text-[var(--fg-muted)]">{empty}</p>;
	}
	return (
		<table className="w-full border-collapse text-left text-[length:var(--text-caption,12px)]">
			<thead>
				<tr className="text-[length:var(--text-micro)] uppercase tracking-[0.08em] text-[var(--fg-muted)]">
					<th className="py-1.5 pr-2 font-medium">File</th>
					{owners && <th className="py-1.5 pr-2 font-medium">Account</th>}
					<th className="py-1.5 pr-2 font-medium">State</th>
					<th className="py-1.5 pr-2 font-medium">Expires</th>
					<th className="py-1.5 pr-2 font-medium">Opens</th>
					<th className="py-1.5 pr-2 font-medium">Downloads</th>
					<th className="py-1.5 pr-2 font-medium">Last opened</th>
					<th className="py-1.5 font-medium" />
				</tr>
			</thead>
			<tbody>
				{links.map((l) => (
					<tr key={l.linkId} data-link-id={l.linkId} data-link-state={l.state} className="border-t border-[var(--border-soft)]">
						<td className="max-w-[200px] truncate py-1.5 pr-2 text-[var(--fg)]" title={l.path}>
							{l.name}
							{l.hasPasscode && <Kv className="ml-1.5">passcode</Kv>}
						</td>
						{owners && (
							<td className="py-1.5 pr-2 text-[var(--fg-muted)]">
								{'ownerUsername' in l ? (l.ownerUsername ?? '—') : '—'}
							</td>
						)}
						<td className="py-1.5 pr-2">{LINK_STATE_LABEL[l.state]}</td>
						<td className="py-1.5 pr-2 text-[var(--fg-muted)]">{expiryText(l.expiresAt, now)}</td>
						<td className="py-1.5 pr-2 font-mono">{l.openCount}</td>
						<td className="py-1.5 pr-2 font-mono">{l.downloadCount}</td>
						<td className="py-1.5 pr-2 text-[var(--fg-muted)]">{relativeTime(l.lastOpenedAt, now)}</td>
						<td className="py-1.5 text-right">
							{canRevoke(l) &&
								(confirming === l.linkId ? (
									<span className="inline-flex gap-1">
										<Button
											type="button"
											size="xs"
											variant="outline"
											className={D05_DANGER}
											onClick={() => {
												setConfirming(null);
												void onRevoke(l.linkId);
											}}
										>
											Revoke now
										</Button>
										<Button type="button" size="xs" variant="outline" onClick={() => setConfirming(null)}>
											Keep
										</Button>
									</span>
								) : (
									<Button type="button" size="xs" variant="outline" onClick={() => setConfirming(l.linkId)}>
										Revoke
									</Button>
								))}
						</td>
					</tr>
				))}
			</tbody>
		</table>
	);
}

function MyLinks() {
	const [links, setLinks] = useState<FileLinkView[] | null>(null);
	const [error, setError] = useState<string | null>(null);
	const load = useCallback(async () => {
		try {
			setLinks((await accessFileLinksList()).links);
			setError(null);
		} catch (e) {
			setError(parseAccessError(e).message);
		}
	}, []);
	useEffect(() => {
		void load();
	}, [load]);
	const revoke = async (linkId: string) => {
		try {
			await accessFileLinkRevoke(linkId);
			await load();
		} catch (e) {
			setError(parseAccessError(e).message);
		}
	};
	return (
		<PeopleBlock
			title="Links"
			right={
				<Button type="button" size="xs" variant="ghost" aria-label="Refresh links" onClick={() => void load()}>
					<RefreshCw />
				</Button>
			}
		>
			<div className="py-2" data-panel="my-links">
				<p className="m-0 pb-2 text-[length:var(--text-micro)] text-[var(--fg-muted)]">
					Outside links you made, for people with no account. Make one from a file's Share… menu. A link can't be shown again after you create it.
				</p>
				{error && (
					<p role="alert" className="m-0 pb-2 text-[12px] text-[var(--danger)]">
						{error}
					</p>
				)}
				{links === null ? (
					<p className="m-0 py-3 text-[length:var(--text-caption,12px)] text-[var(--fg-muted)]">Loading…</p>
				) : (
					<LinkRows links={links} onRevoke={revoke} empty="You haven't made any links." />
				)}
			</div>
		</PeopleBlock>
	);
}

function AllLinks() {
	const [links, setLinks] = useState<AdminFileLinkView[] | null>(null);
	const [owner, setOwner] = useState<string | null>(null);
	const [ended, setEnded] = useState(false);
	const [confirmAll, setConfirmAll] = useState(false);
	const [error, setError] = useState<string | null>(null);
	const load = useCallback(async () => {
		try {
			setLinks((await accessFileLinksAdminList({ includeEnded: ended })).links);
			setError(null);
		} catch (e) {
			// A non-admin (or a weak credential) gets nothing here: the server decides.
			setError(parseAccessError(e).message);
			setLinks([]);
		}
	}, [ended]);
	useEffect(() => {
		void load();
	}, [load]);
	const revoke = async (linkId: string) => {
		try {
			await accessFileLinkAdminRevoke({ linkId });
			await load();
		} catch (e) {
			setError(parseAccessError(e).message);
		}
	};
	const revokeAll = async () => {
		if (owner === null) return;
		setConfirmAll(false);
		try {
			await accessFileLinkAdminRevoke({ ownerPrincipalId: owner });
			await load();
		} catch (e) {
			setError(parseAccessError(e).message);
		}
	};
	const shown = filterByOwner(links ?? [], owner);
	const live = shown.filter(canRevoke).length;
	return (
		<PeopleBlock title="All links" right={<Kv>admin</Kv>}>
			<div className="space-y-2 py-2" data-panel="all-links">
				<div className="flex flex-wrap items-center gap-3">
					<label className="flex items-center gap-1.5 text-[length:var(--text-caption,12px)]">
						Account
						<select
							aria-label="Filter by account"
							value={owner ?? ''}
							onChange={(e) => {
								setOwner(e.target.value === '' ? null : e.target.value);
								setConfirmAll(false);
							}}
							className="h-7 rounded-[var(--radius-sm)] border border-[var(--border)] bg-[var(--bg-sunken)] px-2 text-[length:var(--text-caption,12px)] text-[var(--fg)]"
						>
							<option value="">Everyone</option>
							{ownersOf(links ?? []).map((o) => (
								<option key={o.id} value={o.id}>
									{o.name}
								</option>
							))}
						</select>
					</label>
					<label className="flex items-center gap-1.5 text-[length:var(--text-caption,12px)]">
						<input type="checkbox" checked={ended} onChange={(e) => setEnded(e.target.checked)} />
						Include ended
					</label>
					<span className="ml-auto flex items-center gap-1">
						{owner !== null &&
							live > 0 &&
							(confirmAll ? (
								<>
									<Button type="button" size="xs" variant="outline" className={D05_DANGER} onClick={() => void revokeAll()}>
										Revoke {live} now
									</Button>
									<Button type="button" size="xs" variant="outline" onClick={() => setConfirmAll(false)}>
										Keep
									</Button>
								</>
							) : (
								<Button type="button" size="xs" variant="outline" onClick={() => setConfirmAll(true)}>
									Revoke all
								</Button>
							))}
						<Button type="button" size="xs" variant="ghost" aria-label="Refresh all links" onClick={() => void load()}>
							<RefreshCw />
						</Button>
					</span>
				</div>
				{error && (
					<p role="alert" className="m-0 text-[12px] text-[var(--danger)]">
						{error}
					</p>
				)}
				{links === null ? (
					<p className="m-0 py-3 text-[length:var(--text-caption,12px)] text-[var(--fg-muted)]">Loading…</p>
				) : (
					<LinkRows links={shown} onRevoke={revoke} owners empty="No links." />
				)}
			</div>
		</PeopleBlock>
	);
}
