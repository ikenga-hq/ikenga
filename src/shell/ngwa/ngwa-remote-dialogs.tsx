// R57 · Installed: Update and Remove for vault-managed git / npx items
// (frame-workbench-v4.html, `R57 · UPDATE` and `R57 · REMOVE`).
//
// Only an item with a vault-managed remote record (`managed` and source git /
// npx) reaches these; every other item keeps the locked D-02 Update / Remove.
//
//   Update… — both SHAs, the atomic-swap sentence, a trust line for a source
//             outside the signed catalog, the auto-update opt-in, then
//             `obaUpdate(kind, name, {sha})`: exactly the SHA shown is fetched
//             (never HEAD). A pin mismatch is refused with nothing written.
//   Remove… — checking (dependents read from disk) → Linked into · Required
//             by · Vault copy · What to do: unlink N and delete | relink to
//             another copy, then delete | forget (keep every file). None of
//             them offers Undo; the status line says what happened.

import { useCallback, useEffect, useMemo, useState } from 'react';
import { ArrowRight, Download, Folder, Trash2 } from 'lucide-react';
import type { NgwaItem } from '@ikenga/contract';
import {
	Dialog,
	DialogContent,
	DialogDescription,
	DialogFooter,
	DialogHeader,
	DialogTitle,
} from '@/components/ui/dialog';
import { shortSha } from '@/lib/registry/primitives';
import {
	isObaPinMismatch,
	obaDependents,
	obaForget,
	obaRelinkDependents,
	obaSafeDelete,
	obaSetAutoUpdate,
	obaUnlinkOne,
	obaUpdate,
	type ClaudeStoreKind,
	type ObaPin,
	type ObaRelinkRow,
} from '@/lib/tauri-cmd';
import { open as openDialog } from '@/lib/transport/dialog-shim';
import { errText, isDirKind, samePath, scopeKeyOf } from './ngwa-scope-model';

/** What the vault records about a git / npx item. */
export interface NgwaRemoteRecord {
	kind: ClaudeStoreKind;
	name: string;
	source: 'git' | 'npx';
	url: string;
	/** The resolved SHA (or npm version) installed now. */
	sha: string | null;
	/** The requested git ref; null = the default branch. */
	ref: string | null;
	fromCatalog: boolean;
	/** Installed against a reviewed pin (N-C). */
	pinned: boolean;
	hash: string | null;
	autoUpdate: boolean;
	/** The real master — the vault copy. */
	master: string | null;
	/** Last resolve, epoch ms. */
	resolvedAtMs: number | null;
	/** The signed catalog's pin for it, when it came from the catalog and the
	 *  catalog pins it. Updates then move only to this pin. */
	catalogPin: ObaPin | null;
	/** With a catalog pin: the installed copy is behind it (pure comparison). */
	catalogBehind: boolean;
	/** Links into the vault copy the snapshot saw (the Update confirm's count). */
	links: number;
}

export interface RemoteUpdateRequest {
	item: NgwaItem;
	record: NgwaRemoteRecord;
	/** What Update fetches: the SHA shown (and, from the catalog, its hash). */
	target: ObaPin & { sha: string | null };
	/** The number of links that keep pointing at the vault copy. */
	links: number;
}

export interface RemoteRemoveRequest {
	item: NgwaItem;
	record: NgwaRemoteRecord;
}

export type RemoteDialogResult = { ok: true; text: string } | { ok: false; text: string } | null;

function when(ms: number | null): string {
	if (!ms) return 'at an unrecorded time';
	return new Date(ms).toLocaleDateString(undefined, {
		year: 'numeric',
		month: 'short',
		day: 'numeric',
	});
}

// ─── Update ──────────────────────────────────────────────────────────────────

export function RemoteUpdateDialog({
	request,
	onClose,
	onChanged,
	onRecheck,
}: {
	request: RemoteUpdateRequest | null;
	onClose: (result: RemoteDialogResult) => void;
	/** Invalidate whatever reads the vault (after a write, worked or not). */
	onChanged: () => void;
	/** After a pin mismatch: ask the remote (or the catalog) again. */
	onRecheck: (record: NgwaRemoteRecord) => void;
}) {
	return (
		<Dialog
			open={request !== null}
			onOpenChange={(o) => {
				if (!o) onClose(null);
			}}
		>
			{request && (
				<RemoteUpdateBody
					key={`${request.record.kind}:${request.record.name}`}
					request={request}
					onClose={onClose}
					onChanged={onChanged}
					onRecheck={onRecheck}
				/>
			)}
		</Dialog>
	);
}

function RemoteUpdateBody({
	request,
	onClose,
	onChanged,
	onRecheck,
}: {
	request: RemoteUpdateRequest;
	onClose: (result: RemoteDialogResult) => void;
	onChanged: () => void;
	onRecheck: (record: NgwaRemoteRecord) => void;
}) {
	const { item, record: R, target, links } = request;
	const a = shortSha(R.sha);
	const b = shortSha(target.sha ?? null);
	const [auto, setAuto] = useState(R.autoUpdate);
	const [busy, setBusy] = useState(false);
	const [error, setError] = useState<{ text: string; mismatch: boolean } | null>(null);
	const label = item.display_name || item.name;

	async function toggleAuto(v: boolean) {
		setAuto(v);
		try {
			await obaSetAutoUpdate(R.kind, R.name, v);
			onChanged();
		} catch (e) {
			setAuto(!v);
			setError({ text: `Auto-update was not changed: ${errText(e)}`, mismatch: false });
		}
	}

	async function run() {
		setBusy(true);
		setError(null);
		try {
			await obaUpdate(R.kind, R.name, target);
			onClose({
				ok: true,
				text: `${label} updated ${a} → ${b} — ${links} ${links === 1 ? 'link follows' : 'links follow'} it`,
			});
		} catch (e) {
			setError({ text: errText(e), mismatch: isObaPinMismatch(e) });
		} finally {
			setBusy(false);
			onChanged();
		}
	}

	return (
		<DialogContent data-ngwa-confirm data-ngwa-remote data-remote-update showCloseButton={false}>
			<DialogHeader>
				<DialogTitle>
					<Download className="h-4 w-4 inline mr-1.5" />
					Update {label}
				</DialogTitle>
				<DialogDescription asChild>
					<div className="ngwa-confirm-body">
						<div className="drow">
							<span className="k2">Source</span>
							<span className="val mono">
								{R.source} · {R.url}
							</span>
							<span className="rt meta">
								{R.fromCatalog ? 'curated catalog' : 'added from a URL'}
							</span>
						</div>
						<div className="drow">
							<span className="k2">Installed</span>
							<span className="val mono">{a}</span>
							<span className="rt meta">resolved {when(R.resolvedAtMs)}</span>
						</div>
						<div className="drow">
							<span className="k2">{R.catalogPin ? 'Catalog pin' : 'At the remote'}</span>
							<span className="val mono yes">{b}</span>
							<span className="rt meta">
								{R.catalogPin
									? 'moved by the signed catalog'
									: `HEAD of ${R.ref ?? 'the default branch'}`}
							</span>
						</div>
						<div className="drow">
							<span className="k2">Links</span>
							<span className="val">{links} — they keep pointing at the vault copy</span>
							<span className="rt meta">nothing to relink</span>
						</div>
						<p data-swap>
							Update fetches <b>{b}</b> into a staging folder and swaps it in one step. If the fetch
							fails, <b>{a}</b> stays exactly as it is.
						</p>
						{!R.fromCatalog && (
							<p className="warn" data-trust-line>
								This source is not in the signed catalog. Nobody but you reviews what changed
								between {a} and {b}.
							</p>
						)}
						<label className="ackrow">
							<input
								type="checkbox"
								data-auto
								checked={auto}
								disabled={busy}
								onChange={(e) => void toggleAuto(e.target.checked)}
							/>
							<span>
								Keep {label} updated automatically.{' '}
								{R.fromCatalog
									? 'Catalog installs start with this on.'
									: 'Direct installs start with this off.'}
							</span>
						</label>
						{error && (
							<p className="bad" role="alert" data-update-error>
								{error.mismatch ? (
									<>
										The remote no longer serves <b>{b}</b> — nothing was written, and {a} is
										unchanged. Re-check the remote to see what it serves now.{' '}
									</>
								) : (
									'Update failed: '
								)}
								<code>{error.text}</code>
							</p>
						)}
					</div>
				</DialogDescription>
			</DialogHeader>
			<DialogFooter>
				{error?.mismatch ? (
					<button
						type="button"
						className="chip on"
						data-recheck
						onClick={() => {
							onRecheck(R);
							onClose(null);
						}}
					>
						Re-check the remote
					</button>
				) : (
					<button
						type="button"
						className="chip on"
						disabled={busy}
						aria-busy={busy || undefined}
						onClick={() => void run()}
					>
						{busy ? `Fetching ${b} · swapping in place` : `Update to ${b}`}
					</button>
				)}
				<button type="button" className="chip" disabled={busy} onClick={() => onClose(null)}>
					Keep {a}
				</button>
			</DialogFooter>
		</DialogContent>
	);
}

// ─── Remove ──────────────────────────────────────────────────────────────────

type Choice = 'unlink' | 'relink' | 'forget';

/** A same-named copy elsewhere on disk that links could point at instead. */
export interface RelinkCandidate {
	path: string;
	label: string;
}

/**
 * The copies a Relink could move links to: the same kind+name placed in
 * another scope as a real folder / file (not a link into the vault).
 */
export function relinkCandidates(
	items: readonly NgwaItem[],
	record: NgwaRemoteRecord,
	scopeLabel: (key: string) => string,
	target: (p: NgwaItem['placements'][number]) => string
): RelinkCandidate[] {
	const out: RelinkCandidate[] = [];
	for (const it of items) {
		if (it.name !== record.name) continue;
		for (const p of it.placements) {
			if (p.engine !== 'claude' || !p.present || p.in_store || p.link_target) continue;
			const path = target(p);
			if (record.master && samePath(path, record.master)) continue;
			if (out.some((c) => samePath(c.path, path))) continue;
			out.push({ path, label: scopeLabel(scopeKeyOf(p.scope)) });
		}
	}
	return out;
}

export function RemoteRemoveDialog({
	request,
	items,
	scopeLabel,
	placementPath,
	onClose,
	onChanged,
	onForgotten,
}: {
	request: RemoteRemoveRequest | null;
	/** The snapshot — link scopes, required-by and relink candidates. */
	items: readonly NgwaItem[];
	scopeLabel: (key: string) => string;
	placementPath: (p: NgwaItem['placements'][number]) => string;
	onClose: (result: RemoteDialogResult) => void;
	onChanged: () => void;
	/** Q6: the item now shows as `local`. */
	onForgotten: (record: NgwaRemoteRecord) => void;
}) {
	return (
		<Dialog
			open={request !== null}
			onOpenChange={(o) => {
				if (!o) onClose(null);
			}}
		>
			{request && (
				<RemoteRemoveBody
					key={`${request.record.kind}:${request.record.name}`}
					request={request}
					items={items}
					scopeLabel={scopeLabel}
					placementPath={placementPath}
					onClose={onClose}
					onChanged={onChanged}
					onForgotten={onForgotten}
				/>
			)}
		</Dialog>
	);
}

function RemoteRemoveBody({
	request,
	items,
	scopeLabel,
	placementPath,
	onClose,
	onChanged,
	onForgotten,
}: {
	request: RemoteRemoveRequest;
	items: readonly NgwaItem[];
	scopeLabel: (key: string) => string;
	placementPath: (p: NgwaItem['placements'][number]) => string;
	onClose: (result: RemoteDialogResult) => void;
	onChanged: () => void;
	onForgotten: (record: NgwaRemoteRecord) => void;
}) {
	const { item, record: R } = request;
	const label = item.display_name || item.name;
	const [links, setLinks] = useState<string[] | null>(null);
	const [checkError, setCheckError] = useState<string | null>(null);
	const [choice, setChoice] = useState<Choice>('unlink');
	const [ack, setAck] = useState(false);
	const [busy, setBusy] = useState<string | null>(null);
	const [note, setNote] = useState<{ tone: 'ok' | 'bad'; text: string } | null>(null);
	const [relinkRows, setRelinkRows] = useState<ObaRelinkRow[] | null>(null);

	const recheck = useCallback(async () => {
		setCheckError(null);
		try {
			setLinks(await obaDependents(R.kind, R.name));
		} catch (e) {
			setCheckError(errText(e));
		}
	}, [R.kind, R.name]);
	useEffect(() => {
		void recheck();
	}, [recheck]);

	// Required by: every scope's copy of the item, inverted from requires[].
	const requiredBy = useMemo(() => {
		const seen = new Map<string, { name: string; kind: string }>();
		for (const it of items) {
			if (it.name !== R.name) continue;
			for (const r of it.required_by)
				seen.set(`${r.kind}:${r.name}`, { name: r.name, kind: String(r.kind) });
		}
		return [...seen.values()];
	}, [items, R.name]);

	const candidates = useMemo(
		() => relinkCandidates(items, R, scopeLabel, placementPath),
		[items, R, scopeLabel, placementPath]
	);
	const [relinkTo, setRelinkTo] = useState<string | null>(null);
	const target = relinkTo ?? candidates[0]?.path ?? null;

	/** A link's scope, from the scan; the path itself is what is shown. */
	function linkScope(path: string): string {
		for (const it of items) {
			for (const p of it.placements) {
				if (samePath(p.path, path) || samePath(placementPath(p), path)) {
					const s = scopeLabel(scopeKeyOf(p.scope));
					return p.engine === 'claude' ? s : `${s} · ${p.engine}`;
				}
			}
		}
		return 'link';
	}

	async function unlinkOne(path: string) {
		setNote(null);
		try {
			await obaUnlinkOne(path);
			setLinks((ls) => (ls ?? []).filter((l) => l !== path));
			setNote({ tone: 'ok', text: `Unlinked ${path}` });
		} catch (e) {
			setNote({ tone: 'bad', text: `Unlink failed: ${errText(e)}` });
		} finally {
			onChanged();
		}
	}

	async function pickFolder() {
		try {
			const picked = await openDialog({
				directory: isDirKind(R.kind),
				multiple: false,
				title: `Choose a ${R.kind} named ${R.name}`,
			});
			const path = Array.isArray(picked) ? picked[0] : picked;
			if (path) setRelinkTo(path);
		} catch (e) {
			setNote({ tone: 'bad', text: `Couldn't open the folder picker: ${errText(e)}` });
		}
	}

	/** Delete the vault copy once its links are dealt with; branch on the verdict. */
	async function deleteMaster(done: string) {
		setBusy('Deleting the vault copy');
		const outcome = await obaSafeDelete(R.kind, R.name);
		if (outcome.verdict === 'deleted' || outcome.verdict === 'unlinked') {
			onClose({ ok: true, text: done });
			return;
		}
		if (outcome.verdict === 'refused_external') {
			setNote({
				tone: 'bad',
				text: 'External masters are never deleted by Ngwa. It lives outside the vault — manage it where it is published.',
			});
			return;
		}
		// refused_dependents: a link appeared mid-flow. Re-list, keep the choice.
		setLinks(outcome.dependents);
		setNote({
			tone: 'bad',
			text: `Not deleted: ${outcome.dependents.length} ${outcome.dependents.length === 1 ? 'link' : 'links'} appeared while removing. They are listed again above; your choice is kept.`,
		});
	}

	async function confirm() {
		if (links === null) return;
		const n = links.length;
		setNote(null);
		setRelinkRows(null);
		try {
			if (choice === 'forget') {
				setBusy('Forgetting');
				await obaForget(R.kind, R.name);
				onForgotten(R);
				onClose({
					ok: true,
					text: `Forgot ${label} — its files and ${n} ${n === 1 ? 'link are' : 'links are'} untouched`,
				});
				return;
			}
			if (choice === 'relink') {
				if (!target) return;
				setBusy(`Relinking ${n}`);
				const rows = await obaRelinkDependents(links, target);
				const failed = rows.filter((r) => !r.ok);
				if (failed.length) {
					setRelinkRows(rows);
					setNote({
						tone: 'bad',
						text: `${failed.length} of ${rows.length} links could not be relinked, so the vault copy was kept.`,
					});
					await recheck();
					return;
				}
				await deleteMaster(`Relinked ${n} to ${target} and deleted the vault copy`);
				return;
			}
			for (const l of links) {
				setBusy(`Unlinking ${n}`);
				await obaUnlinkOne(l);
			}
			await deleteMaster(
				`Removed ${label}${n ? ` — ${n} ${n === 1 ? 'link' : 'links'} unlinked, vault copy deleted` : ' — vault copy deleted'}`
			);
		} catch (e) {
			setNote({ tone: 'bad', text: errText(e) });
			await recheck();
		} finally {
			setBusy(null);
			onChanged();
		}
	}

	const n = links?.length ?? 0;
	const needAck = choice === 'unlink' && requiredBy.length > 0;
	const checking = links === null;
	const relinkOff = n === 0;
	const confirmLabel = checking
		? 'Remove'
		: choice === 'unlink'
			? n
				? `Unlink ${n} and delete`
				: 'Delete'
			: choice === 'relink'
				? `Relink ${n} and delete`
				: 'Forget';
	const blocked =
		checking ||
		(needAck && !ack) ||
		(choice === 'relink' && (!target || relinkOff)) ||
		busy !== null;

	const opt = (
		k: Choice,
		c1: string,
		c2: React.ReactNode,
		extra?: React.ReactNode,
		off?: boolean
	) => (
		<label
			className={`choice${choice === k ? ' on' : ''}`}
			data-choice={k}
			aria-disabled={off || undefined}
		>
			<input
				type="radio"
				name="ngwa-remove"
				value={k}
				checked={choice === k}
				disabled={off || busy !== null}
				onChange={() => setChoice(k)}
			/>
			<span style={{ flex: 1, minWidth: 0 }}>
				<span className="c1">{c1}</span>
				<span className="c2">{c2}</span>
				{choice === k && extra}
			</span>
		</label>
	);

	return (
		<DialogContent
			data-ngwa-confirm
			data-ngwa-remote
			data-remote-remove
			className="wide"
			showCloseButton={false}
		>
			<DialogHeader>
				<DialogTitle>
					<Trash2 className="h-4 w-4 inline mr-1.5" />
					Remove {label}
				</DialogTitle>
				<DialogDescription asChild>
					<div className="ngwa-confirm-body">
						{checking ? (
							<div data-remove-checking>
								<p>
									<span className="emberbar">
										<i />
										Finding everything that points at {label}…
									</span>
								</p>
								<p>
									Every scope and every engine is read from disk, not from a stored list, so a lost
									install record cannot hide a link.
								</p>
								{checkError && (
									<p className="bad" role="alert">
										Couldn't read the links: <code>{checkError}</code>
									</p>
								)}
							</div>
						) : (
							<>
								<p>
									<b>{label}</b> lives once in the vault and is linked into{' '}
									{n ? `${n} ${n === 1 ? 'place' : 'places'}` : 'nothing'}. Ngwa never deletes a
									folder that something still points at, so each link is dealt with first.
								</p>

								<div className="subhead first">Linked into · {n}</div>
								<div data-linked>
									{n ? (
										links.map((path) => (
											<div className="drow" key={path} data-link={path}>
												<span className="k2">{linkScope(path)}</span>
												<span className="val mono">{path}</span>
												<span className="rt">
													<button
														type="button"
														className="chip"
														data-unlink={path}
														disabled={busy !== null}
														onClick={() => void unlinkOne(path)}
													>
														Unlink
													</button>
												</span>
											</div>
										))
									) : (
										<div className="drow">
											<span className="k2">—</span>
											<span className="val no">no links left — the vault copy stands alone</span>
										</div>
									)}
								</div>

								<div className="subhead">Required by · {requiredBy.length}</div>
								<div data-required-by>
									{requiredBy.length ? (
										requiredBy.map((r) => (
											<div className="drow" key={`${r.kind}:${r.name}`}>
												<span className="k2 mono">{r.name}</span>
												<span className="val warn">{r.kind} — lists it in requires[]</span>
											</div>
										))
									) : (
										<div className="drow">
											<span className="k2">—</span>
											<span className="val no">nothing lists it in requires[]</span>
										</div>
									)}
								</div>

								<div className="subhead">Vault copy</div>
								<div className="drow" data-vault-copy>
									<span className="k2">Master</span>
									<span className="val mono">{R.master ?? '—'}</span>
									<span className="rt meta">managed · {R.source}</span>
								</div>

								<div className="subhead">What to do</div>
								{opt(
									'unlink',
									n ? `Unlink ${n} and delete` : 'Delete the vault copy',
									<>
										Removes the links, then the vault copy. This cannot be undone — reinstall from{' '}
										<span className="mono">
											{R.source} · {R.url}
										</span>{' '}
										to get it back.
									</>
								)}
								{opt(
									'relink',
									'Relink to another copy, then delete this one',
									relinkOff
										? 'Nothing links to the vault copy, so there is nothing to relink.'
										: candidates.length || relinkTo
											? `Points every link at a copy you pick, so sessions still find a ${R.name} — just not this one.`
											: `No other copy of ${R.name} was found on disk. Choose a folder that holds one.`,
									<>
										{target && (
											<span className="relinkto" data-relink-target>
												<ArrowRight className="h-3 w-3" />
												{target}
												<span className="note" style={{ marginLeft: 'auto' }}>
													{candidates.find((c) => c.path === target)?.label ?? 'chosen'}
												</span>
											</span>
										)}
										{candidates.length > 1 &&
											candidates
												.filter((c) => c.path !== target)
												.map((c) => (
													<button
														key={c.path}
														type="button"
														className="chip"
														onClick={() => setRelinkTo(c.path)}
													>
														{c.label}: {c.path}
													</button>
												))}
										<button
											type="button"
											className="chip"
											data-pickcopy
											disabled={busy !== null}
											onClick={() => void pickFolder()}
										>
											<Folder className="h-3 w-3" /> Choose another folder…
										</button>
									</>,
									relinkOff
								)}
								{opt(
									'forget',
									'Forget it — keep every file',
									'Drops the install record only. The vault copy and every link stay exactly as they are; Update stops working and it shows up as a local item.'
								)}
								{needAck && (
									<label className="ackrow">
										<input
											type="checkbox"
											data-ack
											checked={ack}
											onChange={(e) => setAck(e.target.checked)}
										/>
										<span>
											<b>{requiredBy.map((r) => r.name).join(', ')}</b> will lose something it
											requires. Remove anyway.
										</span>
									</label>
								)}
								{relinkRows && (
									<div data-relink-failures>
										{relinkRows
											.filter((r) => !r.ok)
											.map((r) => (
												<div className="drow" key={r.link}>
													<span className="val mono">{r.link}</span>
													<span className="rt bad">{r.error}</span>
												</div>
											))}
									</div>
								)}
							</>
						)}
						{busy && (
							<p>
								<span className="emberbar" role="status">
									<i />
									{busy}
								</span>
							</p>
						)}
						{note && (
							<p
								className={note.tone === 'bad' ? 'bad' : 'yes'}
								role={note.tone === 'bad' ? 'alert' : 'status'}
								data-remove-note
							>
								{note.text}
							</p>
						)}
					</div>
				</DialogDescription>
			</DialogHeader>
			<DialogFooter>
				<button
					type="button"
					className="chip"
					disabled={busy !== null}
					onClick={() => onClose(null)}
				>
					Keep it
				</button>
				<button
					type="button"
					className={`chip ${choice === 'forget' ? '' : choice === 'relink' ? 'on' : 'danger'}`}
					data-remove-confirm
					disabled={blocked}
					title={needAck && !ack ? 'Tick the box above first' : undefined}
					aria-busy={busy !== null || undefined}
					onClick={() => void confirm()}
				>
					{confirmLabel}
				</button>
			</DialogFooter>
		</DialogContent>
	);
}
