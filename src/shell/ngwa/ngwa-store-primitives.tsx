// R57 · D-02 addendum — git / npx primitives in the Store.
//
// Three pieces the Store surface composes (frame-workbench-v4.html, blocks
// marked R57):
//   - `CatalogStoreRow`  a signed-catalog entry as an ordinary `.srow`;
//   - `CatalogSheet`     its install sheet: overview → requires closure →
//                        permissions / Share kola → trust → Install ▾ (P12);
//   - `AddUrlSheet`      Add from URL…: empty → resolving → resolved →
//                        installing → done | error, in the same sheet column,
//                        so the Store keeps a single consent surface.
// Every install is pinned to what the sheet showed (the catalog's pin, or the
// resolve result's SHA + hash). A pin mismatch is refused by the backend with
// nothing written, and gets its own state here.

import { useEffect, useId, useMemo, useRef, useState, type ReactNode } from 'react';
import { AlertTriangle, Check, ChevronDown, Link2, Shield, X } from 'lucide-react';
import type { NgwaKind } from '@ikenga/contract';
import type { NgwaCatalogRow } from '@/lib/ngwa/enrichment';
import type {
	PrimitiveInstallOutcome,
	PrimitiveInstallStage,
	StoreInstallScope,
} from '@/lib/ngwa/use-store-install';
import {
	catalogPin,
	resolveCatalogClosure,
	shortSha,
	type ConsentDep,
	type PrimitiveCatalogEntry,
} from '@/lib/registry/primitives';
import {
	isObaPinMismatch,
	type ClaudeStoreKind,
	type ClaudeStoreScope,
	type ResolvedSource,
} from '@/lib/tauri-cmd';
import { NOT_AVAILABLE_ON_SERVER_YET } from '@/lib/transport/unavailable';
import { kindIcon } from './ngwa-list';

function errText(e: unknown): string {
	return e instanceof Error ? e.message : String(e);
}

const plural = (n: number, one: string, many = `${one}s`) => (n === 1 ? one : many);

// ─── Closure (shared by the catalog sheet and the URL sheet) ─────────────────

/** How each dep of the closure resolves, in the design's words. */
const RES_WORD = {
	catalog: 'signed catalog',
	pinned: 'not in the catalog · self-pinned',
	unresolved: 'unresolved · no source',
	satisfied: 'already installed · not fetched',
} as const;
const RES_CLS = { catalog: 'yes', pinned: 'warn', unresolved: 'bad', satisfied: 'no' } as const;

type DepRes = keyof typeof RES_WORD;

function depRes(d: ConsentDep): DepRes {
	return d.satisfied ? 'satisfied' : d.resolution;
}

/** Where a dep is fetched from, for the closure row and its consent line. */
export function depProvenance(d: ConsentDep): string {
	if (d.resolution === 'catalog') return d.provenance;
	if (d.resolution === 'pinned') return d.provenance.replace(/ · not in catalog$/, '');
	return 'no source';
}

/** Deps that need their own Share-kola box: fetched, and not from the catalog. */
export function extraConsentDeps(deps: readonly ConsentDep[]): ConsentDep[] {
	return deps.filter((d) => !d.satisfied && d.needsExtraConfirm);
}

/** "no requires" / "also installs 2 skills" — the row's closure tag. */
export function catalogClosureLabel(deps: readonly ConsentDep[]): string {
	if (deps.length === 0) return 'no requires';
	const fetched = deps.filter((d) => !d.satisfied);
	if (fetched.length === 0) return `requires ${deps.length} · all installed`;
	const kinds = new Set(fetched.map((d) => d.kind));
	const noun =
		kinds.size === 1 ? plural(fetched.length, [...kinds][0]) : plural(fetched.length, 'item');
	return `also installs ${fetched.length} ${noun}`;
}

function ClosureRows({ deps }: { deps: readonly ConsentDep[] }) {
	return (
		<div data-closure-rows>
			{deps.map((d) => {
				const res = depRes(d);
				return (
					<div className="drow" key={`${d.kind}:${d.name}`} data-dep={d.name} data-res={res}>
						<span className="k2 mono">{d.name}</span>
						<span className={`val ${RES_CLS[res]}`}>{RES_WORD[res]}</span>
						{res !== 'satisfied' && <span className="rt meta mono">{depProvenance(d)}</span>}
					</div>
				);
			})}
		</div>
	);
}

// ─── Install ▾ (P12) ─────────────────────────────────────────────────────────

/** The consent gate's reason, with a live count so a disabled Install reads
 *  as "not yet", not "broken" — e.g. "Tick every consent above first (2 of 6
 *  ticked)". */
export function consentBlockedReason(
	ticked: number,
	total: number,
	text = 'Tick every consent above first'
): string {
	return `${text} (${ticked} of ${total} ticked)`;
}

/** The sheet foot's split Install button: the active project by default, the
 *  caret offers personal. Shared by the registry, catalog and URL sheets.
 *  While blocked, the reason is written out beside it (not only a tooltip).
 *  A null `projectLabel` means there is no project target — a pkg while the
 *  Default project is active (DEC-71) — so personal is the only target. */
export function InstallSplit({
	projectLabel,
	blocked,
	busy,
	onInstall,
	verb = 'Install',
}: {
	projectLabel: string | null;
	/** Why Install can't run yet, or null. */
	blocked: string | null;
	busy: boolean;
	onInstall: (scope: StoreInstallScope) => void;
	/** The action's verb: `Reinstall` for a pkg on disk that failed to load. */
	verb?: 'Install' | 'Reinstall';
}) {
	const [menuOpen, setMenuOpen] = useState(false);
	const menuRef = useRef<HTMLDivElement | null>(null);
	const reasonId = useId();
	useEffect(() => {
		if (!menuOpen) return;
		function onKey(e: KeyboardEvent) {
			if (e.key === 'Escape') {
				e.preventDefault();
				setMenuOpen(false);
			}
		}
		function onDown(e: MouseEvent) {
			if (menuRef.current?.contains(e.target as Node)) return;
			setMenuOpen(false);
		}
		document.addEventListener('keydown', onKey);
		document.addEventListener('mousedown', onDown);
		return () => {
			document.removeEventListener('keydown', onKey);
			document.removeEventListener('mousedown', onDown);
		};
	}, [menuOpen]);
	function go(scope: StoreInstallScope) {
		setMenuOpen(false);
		if (blocked || busy) return;
		onInstall(scope);
	}
	const describedBy = blocked && !busy ? reasonId : undefined;
	// The primary button's target: the active project, or personal when there
	// is no project target.
	const primary: StoreInstallScope = projectLabel === null ? 'personal' : 'project';
	return (
		<>
			<div className="installsplit" ref={menuRef}>
				<button
					type="button"
					className="btn primary lg"
					data-install
					disabled={Boolean(blocked) || busy}
					aria-busy={busy || undefined}
					aria-describedby={describedBy}
					title={blocked ?? undefined}
					onClick={() => go(primary)}
				>
					{verb} to {projectLabel ?? 'personal'}
				</button>
				<button
					type="button"
					className="btn primary lg caret"
					aria-label="Choose install scope"
					aria-haspopup="menu"
					aria-expanded={menuOpen}
					title={blocked ?? 'Choose an install scope'}
					disabled={Boolean(blocked) || busy}
					onClick={() => setMenuOpen((o) => !o)}
				>
					<ChevronDown className="h-3.5 w-3.5" />
				</button>
				{menuOpen && (
					<div className="cellpop storepop up" role="menu" aria-label="Install scope">
						<div className="mgroup">Install scope</div>
						{projectLabel !== null && (
							<button type="button" role="menuitem" className="mitem" onClick={() => go('project')}>
								{verb} to {projectLabel} <span className="msub">default here</span>
							</button>
						)}
						<button type="button" role="menuitem" className="mitem" onClick={() => go('personal')}>
							{verb} to personal{' '}
							<span className="msub">
								{projectLabel === null ? 'default here · ' : ''}workspace · always loaded
							</span>
						</button>
					</div>
				)}
			</div>
			{describedBy && (
				<span className="note" id={reasonId} data-install-blocked>
					{blocked}
				</span>
			)}
		</>
	);
}

/** One Share-kola checkbox. */
function Consent({
	id,
	b,
	d,
	checked,
	disabled,
	onChange,
}: {
	id: string;
	b: ReactNode;
	d: ReactNode;
	checked: boolean;
	disabled?: boolean;
	onChange: (v: boolean) => void;
}) {
	return (
		<div className="consent">
			<label>
				<input
					type="checkbox"
					data-consent={id}
					checked={checked}
					disabled={disabled}
					onChange={(e) => onChange(e.target.checked)}
				/>
				<span>
					<span className="b">{b}</span> <span className="d">{d}</span>
				</span>
			</label>
		</div>
	);
}

/** Where a placed primitive lands in a scope, for the done list. */
function placedPath(scope: ClaudeStoreScope, scopeLabel: string, kind: string, name: string) {
	const root = scope === 'workspace' ? '~' : scopeLabel;
	if (kind === 'hook' || kind === 'mcp') return `${root}/.claude/settings.json`;
	return `${root}/.claude/${kind}s/${name}`;
}

/** The done state's body: what was placed and where, and what was left. */
function PlacedList({
	outcome,
	scopeLabel,
}: {
	outcome: PrimitiveInstallOutcome;
	scopeLabel: string;
}) {
	return (
		<div data-placed>
			<div className="subhead">Placed</div>
			{outcome.placed.map((p) => (
				<div className="drow" key={`p:${p.kind}:${p.name}`} data-placed-item={p.name}>
					<span className="k2 mono">{p.name}</span>
					<span className="val mono">{placedPath(outcome.scope, scopeLabel, p.kind, p.name)}</span>
					<span className="rt meta">→ vault</span>
				</div>
			))}
			{outcome.alsoEnabled.map((p) => (
				<div className="drow" key={`e:${p.kind}:${p.name}`} data-also-enabled={p.name}>
					<span className="k2 mono">{p.name}</span>
					<span className="val">already installed — also enabled in {scopeLabel}</span>
				</div>
			))}
			{outcome.leftInPlace.map((p) => (
				<div className="drow" key={`l:${p.kind}:${p.name}`} data-left={p.name}>
					<span className="k2 mono">{p.name}</span>
					<span className="val no">already installed — left where it is</span>
				</div>
			))}
		</div>
	);
}

/** "Installed X to Y — plus N required items". */
function doneTitle(name: string, scopeLabel: string, outcome: PrimitiveInstallOutcome): string {
	const extra = outcome.placed.length - 1 + outcome.alsoEnabled.length;
	return `Installed ${name} to ${scopeLabel}${extra > 0 ? ` — plus ${extra} required ${plural(extra, 'item')}` : ''}`;
}

function scopeLabelOf(scope: ClaudeStoreScope, projectLabel: string): string {
	return scope === 'workspace' ? 'personal' : projectLabel;
}

// ─── Catalog row ─────────────────────────────────────────────────────────────

export function CatalogStoreRow({
	row,
	closure,
	selected,
	busy,
	onSelect,
	onUpdate,
	disabledReason,
}: {
	row: NgwaCatalogRow;
	closure: readonly ConsentDep[];
	selected: boolean;
	busy: boolean;
	onSelect: () => void;
	onUpdate?: (row: NgwaCatalogRow) => void;
	disabledReason?: string;
}) {
	return (
		// biome-ignore lint/a11y/useSemanticElements: the same `.srow` as a registry row — it holds its own Install / Update button, so it can't be a <button>
		<div
			className={`srow ${selected ? 'sel' : ''}`}
			data-id={row.id}
			data-catalog-row
			onClick={onSelect}
			role="button"
			tabIndex={0}
			aria-pressed={selected}
			aria-busy={busy || undefined}
			onKeyDown={(e) => {
				if (e.target !== e.currentTarget) return;
				if (e.key === 'Enter' || e.key === ' ') {
					e.preventDefault();
					onSelect();
				}
			}}
		>
			<div className="mark">{kindIcon(row.kind as NgwaKind)}</div>
			<div className="mid">
				<div className="l1">
					<span className="pkg">{row.name}</span>
					<span className={`kind k-${row.kind}`}>{row.kind}</span>
					<span className="meta mono">{row.version}</span>
					{row.publisher && <span className="meta">{row.publisher}</span>}
					<span className="badge t-unsigned">
						<Shield className="h-3 w-3" />
						unsigned
					</span>
				</div>
				<div className="l2">{row.description ?? 'No description.'}</div>
				<div className="l3" data-l3>
					<span className="tagp" data-closure>
						{catalogClosureLabel(closure)}
					</span>
					<span className="tagp mono" data-source-tag>
						{row.source} · {row.url}
					</span>
					<span className="tagp" data-signed-catalog>
						in the signed catalog
					</span>
				</div>
			</div>
			<div className="rt">
				{row.isUpdate ? (
					<button
						type="button"
						className="btn"
						disabled={!onUpdate || busy}
						title={onUpdate ? undefined : (disabledReason ?? NOT_AVAILABLE_ON_SERVER_YET)}
						onClick={(e) => {
							e.stopPropagation();
							onUpdate?.(row);
						}}
					>
						{busy ? 'Updating…' : 'Update'}
					</button>
				) : row.installed ? (
					<span className="badge t-builtin">
						<Check className="h-3 w-3" /> installed
					</span>
				) : (
					<button
						type="button"
						className="btn primary"
						onClick={(e) => {
							e.stopPropagation();
							onSelect();
						}}
					>
						Install
					</button>
				)}
			</div>
		</div>
	);
}

// ─── Catalog sheet ───────────────────────────────────────────────────────────

type CatalogRun =
	| { step: 'idle' }
	| { step: 'installing'; stage: PrimitiveInstallStage; scope: StoreInstallScope }
	| { step: 'done'; outcome: PrimitiveInstallOutcome }
	| { step: 'error'; error: string; mismatch: boolean };

export function CatalogSheet({
	row,
	closure,
	projectLabel,
	onClose,
	onInstall,
	onUpdate,
	updating,
	updateError,
	onRecheckCatalog,
	onOpenInstalled,
	disabledReason,
}: {
	row: NgwaCatalogRow;
	closure: readonly ConsentDep[];
	projectLabel: string;
	onClose: () => void;
	onInstall?: (
		row: NgwaCatalogRow,
		scope: StoreInstallScope,
		onStage: (s: PrimitiveInstallStage) => void
	) => Promise<PrimitiveInstallOutcome>;
	onUpdate?: (row: NgwaCatalogRow) => void;
	updating: boolean;
	updateError: string | null;
	onRecheckCatalog?: () => void;
	onOpenInstalled?: (name: string) => void;
	disabledReason?: string;
}) {
	const e = row.entry;
	const pin = catalogPin(e);
	const settingsKind = e.kind === 'hook' || e.kind === 'mcp';
	const extra = useMemo(() => extraConsentDeps(closure), [closure]);
	const fetched = closure.filter((d) => !d.satisfied);
	const [ticked, setTicked] = useState<Record<string, boolean>>({});
	const [run, setRun] = useState<CatalogRun>({ step: 'idle' });

	const consentIds = [...(settingsKind ? ['runs'] : []), ...extra.map((d) => `dep:${d.name}`)];
	const allTicked = consentIds.every((id) => ticked[id]);
	const busy = run.step === 'installing';

	let blocked: string | null = null;
	if (!onInstall) blocked = disabledReason ?? NOT_AVAILABLE_ON_SERVER_YET;
	else if (!allTicked)
		blocked = consentBlockedReason(consentIds.filter((id) => ticked[id]).length, consentIds.length);

	async function install(scope: StoreInstallScope) {
		if (!onInstall || blocked || busy) return;
		setRun({ step: 'installing', stage: 'fetch', scope });
		try {
			const outcome = await onInstall(row, scope, (stage) =>
				setRun((r) => (r.step === 'installing' ? { ...r, stage } : r))
			);
			setRun({ step: 'done', outcome });
		} catch (err) {
			setRun({ step: 'error', error: errText(err), mismatch: isObaPinMismatch(err) });
		}
	}

	const scopeFor = (s: StoreInstallScope) => (s === 'personal' ? 'personal' : projectLabel);
	const pinText = pin?.sha ? shortSha(pin.sha) : pin?.hash ? pin.hash.slice(0, 19) : null;
	const stageText =
		run.step === 'installing'
			? run.stage === 'fetch'
				? `Fetching ${pinText ?? e.source}${fetched.length ? ' → Materializing deps' : ''}`
				: `Placing in ${scopeFor(run.scope)}`
			: '';

	return (
		<>
			<div className="dhead">
				<div className="dtitle">
					<span className="ico">{kindIcon(row.kind as NgwaKind)}</span>
					<h2>{row.name}</h2>
					<span className={`kind k-${row.kind}`}>{row.kind}</span>
					<span className="v">{row.version}</span>
					<span className="badge t-unsigned">
						<Shield className="h-3 w-3" />
						unsigned
					</span>
					<span style={{ flex: 1 }} />
					<button type="button" className="iconbtn" aria-label="Close sheet" onClick={onClose}>
						<X className="h-3.5 w-3.5" />
					</button>
				</div>
				<div className="dsub">
					{row.publisher && (
						<span>
							publisher <b>{row.publisher}</b>
						</span>
					)}
					<span className="mono">
						{row.source} · {row.url}
					</span>
					<span>
						from the <b>curated catalog</b>
					</span>
				</div>
			</div>

			<div className="sheetbody sc" data-catalog-sheet>
				{run.step === 'done' ? (
					<>
						<div className="trustbox ok" data-install-done>
							<Check className="ico h-3.5 w-3.5" />
							<span>
								<span className="t1">
									{doneTitle(row.name, scopeLabelOf(run.outcome.scope, projectLabel), run.outcome)}
								</span>
								<span className="t2">
									Each one is a vault folder linked into the scope.{' '}
									{pin
										? 'It moves only when the signed catalog moves its pin.'
										: 'Catalog installs follow the source.'}
								</span>
							</span>
						</div>
						<PlacedList
							outcome={run.outcome}
							scopeLabel={scopeLabelOf(run.outcome.scope, projectLabel)}
						/>
					</>
				) : (
					<>
						<div className="subhead first">Overview</div>
						<p className="note" style={{ fontSize: 'var(--text-caption)' }}>
							{row.description ?? 'No description.'}
						</p>

						{closure.length > 0 ? (
							<div data-requires>
								<div className="subhead">Requires — the closure, before you consent</div>
								<ClosureRows deps={closure} />
								<p className="consentnote">
									Read from the catalog, before anything is fetched. The installer re-reads each
									fetched manifest and has the last word.
								</p>
							</div>
						) : (
							<>
								<div className="subhead">Requires</div>
								<div className="drow">
									<span className="k2">Closure</span>
									<span className="val no">nothing — this installs alone</span>
								</div>
							</>
						)}

						<div className="subhead">Permissions</div>
						{settingsKind || extra.length > 0 ? (
							<div data-consents>
								<p className="kola">Share kola</p>
								<p className="note" style={{ marginBottom: 'var(--space-2)' }}>
									{settingsKind
										? `A ${e.kind === 'mcp' ? 'MCP server' : 'hook'} runs code on your machine. Tick to enable Install.`
										: `A ${e.kind} declares intent and never grants itself anything. ${
												extra.length === 1
													? 'One of its dependencies comes from outside the catalog, so that one needs your say-so.'
													: `${extra.length} of its dependencies come from outside the catalog, so each needs your say-so.`
											}`}
								</p>
								{settingsKind && (
									<Consent
										id="runs"
										b={row.name}
										d={
											e.kind === 'mcp'
												? 'is merged into settings.json and starts its server in every session of the scope you pick.'
												: 'is merged into settings.json and runs its command in every session of the scope you pick.'
										}
										checked={Boolean(ticked.runs)}
										disabled={busy}
										onChange={(v) => setTicked((t) => ({ ...t, runs: v }))}
									/>
								)}
								{extra.map((d) => (
									<Consent
										key={d.name}
										id={`dep:${d.name}`}
										b={d.name}
										d={`is not in the signed catalog. It is fetched from the source ${row.name} names for it (${depProvenance(d)}), which nothing here has reviewed.`}
										checked={Boolean(ticked[`dep:${d.name}`])}
										disabled={busy}
										onChange={(v) => setTicked((t) => ({ ...t, [`dep:${d.name}`]: v }))}
									/>
								))}
							</div>
						) : (
							<p className="note">
								A {e.kind} declares intent and never grants itself anything, so there is nothing to
								consent to. Install is enabled.
							</p>
						)}

						<div className="subhead">Trust</div>
						<div className="drow">
							<span className="k2">Catalog entry</span>
							<span className="val yes">signed — vouches for the name and where it comes from</span>
						</div>
						<div className="drow">
							<span className="k2">Files</span>
							<span className="val warn">unsigned — fetched from {e.source} at install</span>
						</div>
						<div className="drow" data-pinned-version>
							<span className="k2">Pinned version</span>
							{pin?.sha ? (
								<>
									<span className="val mono yes">{shortSha(pin.sha)}</span>
									<span className="rt meta">
										{pin.hash ? 'SHA + content hash, signed' : 'signed with the entry'}
									</span>
								</>
							) : pin?.hash ? (
								<>
									<span className="val mono yes">{pin.hash.slice(0, 19)}…</span>
									<span className="rt meta">content hash, verified at install</span>
								</>
							) : (
								<span className="val warn">none — installs whatever the source serves today</span>
							)}
						</div>
						<div className="drow" data-updates-policy>
							<span className="k2">Updates</span>
							<span className="val">
								{pin
									? 'moves only when the signed catalog moves the pin'
									: 'automatic — catalog installs follow the source'}
							</span>
							<span className="rt meta">change in Installed</span>
						</div>

						<div className="subhead">Settings</div>
						<div className="drow">
							<span className="k2">settings.schema</span>
							<span className="val no">
								{settingsKind
									? `none — a ${e.kind} is configured in settings.json`
									: `none — a ${e.kind} is configured by editing its files`}
							</span>
						</div>

						{run.step === 'error' && (
							<div className="trustbox err" role="alert" data-install-error>
								<AlertTriangle className="ico h-3.5 w-3.5" />
								<span>
									<span className="t1">
										{run.mismatch
											? 'The source no longer serves the pinned version'
											: 'The install did not finish'}
									</span>
									<span className="t2">
										<code>{run.error}</code>
									</span>
									<span className="t2">
										{run.mismatch
											? `The signed catalog pins ${pinText ?? 'this entry'}, and the source no longer serves it. Nothing was written. Re-check the catalog: if it moved the pin, install that.`
											: 'Check the source is reachable, then try again.'}
									</span>
								</span>
							</div>
						)}
					</>
				)}
			</div>

			<div className="sheetfoot" data-sheetfoot>
				{busy && (
					<span className="emberbar" role="status" data-stage>
						<i /> {stageText}
					</span>
				)}
				{updating && (
					<span className="emberbar" role="status">
						<i /> Updating to {pinText}
					</span>
				)}
				{updateError && (
					<span className="note bad" role="alert" data-action-error>
						{isObaPinMismatch(updateError)
							? `The source no longer serves ${pinText} — nothing was written. `
							: 'Failed: '}
						{updateError}
					</span>
				)}
				{run.step === 'done' ? (
					onOpenInstalled && (
						<button
							type="button"
							className="btn primary lg"
							onClick={() => onOpenInstalled(row.name)}
						>
							Open in Installed
						</button>
					)
				) : run.step === 'error' && run.mismatch ? (
					<button
						type="button"
						className="btn primary lg"
						data-recheck
						onClick={() => {
							setRun({ step: 'idle' });
							onRecheckCatalog?.();
						}}
					>
						Re-check the catalog
					</button>
				) : row.isUpdate ? (
					<button
						type="button"
						className="btn primary lg"
						disabled={!onUpdate || updating}
						aria-busy={updating || undefined}
						onClick={() => onUpdate?.(row)}
					>
						Update to {pinText}
					</button>
				) : row.installed ? (
					<>
						<span className="badge t-builtin">
							<Check className="h-3 w-3" /> installed
						</span>
						<span className="note">Remove, move or update it from the Installed tab.</span>
					</>
				) : (
					<InstallSplit
						projectLabel={projectLabel}
						blocked={blocked}
						busy={busy}
						onInstall={(s) => void install(s)}
					/>
				)}
				{run.step !== 'done' && !row.installed && (
					<span className="note" style={{ marginLeft: 'auto' }}>
						fetches {fetched.length + 1} · size known after fetch
					</span>
				)}
			</div>
		</>
	);
}

// ─── Add from URL ────────────────────────────────────────────────────────────

/** Strip a pasted `npx skills add ` prefix. */
export function normalizeSource(u: string): string {
	return u.trim().replace(/^npx\s+skills\s+add\s+/i, '');
}

/** True when the source is an `owner/repo` spec (npx), not a git URL. */
export function isNpxSpec(u: string): boolean {
	const s = normalizeSource(u);
	return (
		!!s &&
		!/^(https?:|git@|file:|ssh:)/.test(s) &&
		/^[\w.-]+\/[\w.-]+$/.test(s.replace(/^github:/, ''))
	);
}

/** The vault name a source suggests: its last path segment, `.git` dropped. */
export function nameFromSource(u: string): string {
	return (
		normalizeSource(u)
			.replace(/\.git$/, '')
			.replace(/\/+$/, '')
			.split(/[/:]/)
			.pop() ?? ''
	);
}

type UrlKind = 'infer' | ClaudeStoreKind;
const URL_KINDS: Array<[UrlKind, string]> = [
	['infer', 'Infer'],
	['skill', 'skill'],
	['agent', 'agent'],
	['command', 'command'],
	['hook', 'hook'],
	['mcp', 'mcp'],
];

/** Why a Kind segment is disabled for this source, or null. N-A made hook and
 *  mcp installable from git; npx (`npx skills add`) installs skills only. */
export function urlKindBlock(kind: UrlKind, npx: boolean): string | null {
	if (!npx || kind === 'infer' || kind === 'skill') return null;
	return 'npx installs skills only — use a git URL for agents, commands, hooks and MCP entries';
}

type UrlStep =
	| { step: 'empty' }
	| { step: 'resolving' }
	| { step: 'error'; error: string; mismatch: ResolvedSource | null }
	| { step: 'resolved'; r: ResolvedSource; installError: string | null }
	| {
			step: 'installing';
			r: ResolvedSource;
			stage: PrimitiveInstallStage;
			scope: StoreInstallScope;
	  }
	| { step: 'done'; r: ResolvedSource; outcome: PrimitiveInstallOutcome };

const STAGE_MS = 1200;

export function AddUrlSheet({
	initialSource = '',
	projectLabel,
	catalog,
	installedKeys,
	disabledReason,
	onClose,
	onResolve,
	onInstall,
	onOpenInstalled,
}: {
	/** Carried in from an empty search. */
	initialSource?: string;
	projectLabel: string;
	catalog: readonly PrimitiveCatalogEntry[];
	/** `${kind}:${name}` of the vault, for the closure's "already installed". */
	installedKeys: ReadonlySet<string>;
	disabledReason?: string;
	onClose: () => void;
	onResolve?: (
		url: string,
		opts: { kind: ClaudeStoreKind | null; name: string | null; gitRef: string | null }
	) => Promise<ResolvedSource>;
	onInstall?: (
		resolved: ResolvedSource,
		scope: StoreInstallScope,
		onStage: (s: PrimitiveInstallStage) => void
	) => Promise<PrimitiveInstallOutcome>;
	onOpenInstalled?: (name: string) => void;
}) {
	const [source, setSource] = useState(initialSource);
	const [kind, setKind] = useState<UrlKind>('infer');
	const [name, setName] = useState(nameFromSource(initialSource));
	const [nameTouched, setNameTouched] = useState(false);
	const [ref, setRef] = useState('');
	const [state, setState] = useState<UrlStep>({ step: 'empty' });
	const [resolveStage, setResolveStage] = useState(0);
	const [ticked, setTicked] = useState<Record<string, boolean>>({});
	const sourceRef = useRef<HTMLInputElement | null>(null);

	const src = normalizeSource(source);
	const npx = isNpxSpec(source);
	const locked = state.step === 'resolving' || state.step === 'installing' || state.step === 'done';
	const inputsVisible =
		state.step === 'empty' || state.step === 'resolving' || state.step === 'error';

	useEffect(() => {
		if (state.step === 'empty') sourceRef.current?.focus();
	}, [state.step]);

	// The resolving button walks its stages; the backend call is one command,
	// so the last stage holds until it answers.
	useEffect(() => {
		if (state.step !== 'resolving') return;
		setResolveStage(0);
		const t = setInterval(() => setResolveStage((s) => Math.min(s + 1, 2)), STAGE_MS);
		return () => clearInterval(t);
	}, [state.step]);

	function onSourceChange(v: string) {
		setSource(v);
		if (!nameTouched) setName(nameFromSource(v));
		if (urlKindBlock(kind, isNpxSpec(v))) setKind('infer');
	}

	async function resolve() {
		if (!onResolve || !src || locked) return;
		setState({ step: 'resolving' });
		setTicked({});
		try {
			const r = await onResolve(src, {
				kind: kind === 'infer' ? null : kind,
				name: name.trim() || null,
				gitRef: npx ? null : ref.trim() || null,
			});
			setState({ step: 'resolved', r, installError: null });
		} catch (err) {
			setState({ step: 'error', error: errText(err), mismatch: null });
		}
	}

	async function install(r: ResolvedSource, scope: StoreInstallScope) {
		if (!onInstall) return;
		setState({ step: 'installing', r, stage: 'fetch', scope });
		try {
			const outcome = await onInstall(r, scope, (stage) =>
				setState((s) => (s.step === 'installing' ? { ...s, stage } : s))
			);
			setState({ step: 'done', r, outcome });
		} catch (err) {
			if (isObaPinMismatch(err)) setState({ step: 'error', error: errText(err), mismatch: r });
			else setState({ step: 'resolved', r, installError: errText(err) });
		}
	}

	function addAnother() {
		setSource('');
		setName('');
		setNameTouched(false);
		setRef('');
		setKind('infer');
		setTicked({});
		setState({ step: 'empty' });
	}

	const r =
		state.step === 'resolved' || state.step === 'installing' || state.step === 'done'
			? state.r
			: null;

	const closure = useMemo<ConsentDep[]>(() => {
		if (!r) return [];
		const pseudo: PrimitiveCatalogEntry = {
			kind: r.kind,
			name: r.name,
			version: r.sha ?? '',
			description: r.description,
			source: r.source,
			url: r.url,
			requires: r.requires,
		};
		return resolveCatalogClosure(pseudo, catalog, installedKeys);
	}, [r, catalog, installedKeys]);
	const extra = extraConsentDeps(closure);
	const fetchN = closure.filter((d) => !d.satisfied).length + 1;
	const pinText = r ? (r.sha ? shortSha(r.sha) : r.hash.slice(0, 19)) : '';
	const consentIds = ['src', ...extra.map((d) => `dep:${d.name}`)];
	const allTicked = consentIds.every((id) => ticked[id]);
	const resolveSteps = npx
		? ['npx skills add', 'Locating SKILL.md', 'Reading requires']
		: ['Cloning', `Locating ${kind === 'infer' ? 'the primitive' : kind}`, 'Reading requires'];
	const shortSource = (u: string) =>
		u.replace(/^https?:\/\/(www\.)?github\.com\//, '').replace(/^https?:\/\//, '');

	const header = inputsVisible ? (
		<h2>Add from URL</h2>
	) : (
		<>
			<h2>{r?.name}</h2>
			<span className={`kind k-${r?.kind}`}>{r?.kind}</span>
			<span className="v">{pinText}</span>
			<span className="badge t-review">
				<Shield className="h-3 w-3" />
				review
			</span>
		</>
	);

	// ── bodies ──
	let body: ReactNode;
	let foot: ReactNode;
	if (inputsVisible) {
		const mismatch = state.step === 'error' ? state.mismatch : null;
		body = (
			<>
				<div className="subhead first">Source</div>
				<div className="q2">
					<div className="field" aria-disabled={locked || undefined}>
						<Link2 className="h-3.5 w-3.5" />
						<input
							ref={sourceRef}
							data-urlinput
							aria-label="git URL or npx package"
							placeholder="https://github.com/owner/repo  ·  owner/repo"
							value={source}
							readOnly={locked}
							onChange={(e) => onSourceChange(e.target.value)}
							onKeyDown={(e) => {
								if (e.key === 'Enter') {
									e.preventDefault();
									void resolve();
								}
							}}
						/>
						<span className="suffix" data-route>
							{src ? (npx ? 'npx' : 'git') : ''}
						</span>
					</div>
					<span className="hint">
						A git URL is cloned. <span className="mono">owner/repo</span> runs{' '}
						<span className="mono">npx skills add</span>, which installs skills only. Public sources
						only for now.
					</span>
				</div>

				<div className="subhead">Kind</div>
				<div className="q2">
					<div className="seg" role="radiogroup" aria-label="Kind">
						{URL_KINDS.map(([k, label]) => {
							const why = urlKindBlock(k, npx);
							return (
								// biome-ignore lint/a11y/useSemanticElements: the design's segmented control (`.seg`), a radiogroup of buttons
								<button
									key={k}
									type="button"
									role="radio"
									data-k={k}
									aria-checked={kind === k}
									className={kind === k ? 'on' : undefined}
									disabled={Boolean(why) || locked}
									title={why ?? undefined}
									onClick={() => setKind(k)}
								>
									{label}
								</button>
							);
						})}
					</div>
					<span className="hint">
						{kind === 'infer' ? (
							<>
								Infer reads the fetched tree: a root <span className="mono">SKILL.md</span> is a
								skill, a lone <span className="mono">&lt;name&gt;.md</span> under{' '}
								<span className="mono">agents/</span> or <span className="mono">commands/</span> is
								that kind.
							</>
						) : (
							`Looks for a ${kind} named below in the fetched tree.`
						)}
					</span>
				</div>

				<div className="subhead">Name</div>
				<div className="q2">
					<div className="field" aria-disabled={locked || undefined}>
						<input
							data-urlname
							aria-label="Name"
							placeholder="derived from the URL"
							value={name}
							readOnly={locked}
							onChange={(e) => {
								setName(e.target.value);
								setNameTouched(true);
							}}
						/>
						<span className="suffix">vault folder</span>
					</div>
					<span className="hint">
						Must match the folder or file name inside the source. Derived from the URL; change it
						for a repo that holds several.
					</span>
				</div>

				{!npx && (
					<>
						<div className="subhead">Ref</div>
						<div className="q2">
							<div className="field" aria-disabled={locked || undefined}>
								<input
									data-urlref
									aria-label="Git ref"
									placeholder="default branch"
									value={ref}
									readOnly={locked}
									onChange={(e) => setRef(e.target.value)}
								/>
								<span className="suffix">branch · tag · sha</span>
							</div>
						</div>
					</>
				)}

				{state.step === 'error' ? (
					<div className="trustbox err" role="alert" data-url-error>
						<AlertTriangle className="ico h-3.5 w-3.5" />
						<span>
							<span className="t1">
								{mismatch ? 'The source moved since Resolve' : 'Nothing to install at that name'}
							</span>
							<span className="t2">
								<code data-backend-error>{state.error}</code>
							</span>
							<span className="t2">
								{mismatch
									? `The source no longer serves ${mismatch.sha ? shortSha(mismatch.sha) : 'what was resolved'} — what you reviewed is not what it serves now. Nothing was written. Resolve again to review what it serves today.`
									: 'Is it an agent or a command? Pick the kind. If the repo holds several skills, set Name to the one you want. Nothing was written — the fetch stayed in a staging folder.'}
							</span>
						</span>
					</div>
				) : (
					<p className="consentnote">
						Resolving fetches into a staging folder and reads it. Nothing is placed, and nothing in
						your scopes changes, until you Install.
					</p>
				)}
			</>
		);
		foot = (
			<>
				{state.step === 'resolving' && (
					<span className="emberbar" role="status" data-stage>
						<i /> {resolveSteps[resolveStage]}
					</span>
				)}
				<button
					type="button"
					className="btn primary lg"
					data-resolve
					disabled={!src || !onResolve || state.step === 'resolving'}
					aria-busy={state.step === 'resolving' || undefined}
					title={!src ? 'Paste a URL or an owner/repo first' : undefined}
					onClick={() => void resolve()}
				>
					{state.step === 'error' ? 'Resolve again' : 'Resolve'}
				</button>
				<span className="note" style={{ marginLeft: 'auto' }}>
					{state.step === 'error' ? 'fetch discarded' : 'no network until Resolve'}
				</span>
			</>
		);
	} else if (state.step === 'done' && r) {
		const scopeLabel = scopeLabelOf(state.outcome.scope, projectLabel);
		body = (
			<>
				<div className="trustbox ok" data-install-done>
					<Check className="ico h-3.5 w-3.5" />
					<span>
						<span className="t1">{doneTitle(r.name, scopeLabel, state.outcome)}</span>
						<span className="t2">
							Each one is a vault folder linked into the scope. Auto-update is off because this came
							from a URL.
						</span>
					</span>
				</div>
				<PlacedList outcome={state.outcome} scopeLabel={scopeLabel} />
			</>
		);
		foot = (
			<>
				{onOpenInstalled && (
					<button type="button" className="btn primary lg" onClick={() => onOpenInstalled(r.name)}>
						Open in Installed
					</button>
				)}
				<button type="button" className="btn lg" onClick={addAnother}>
					Add another
				</button>
			</>
		);
	} else if (r) {
		const installing = state.step === 'installing';
		const installError = state.step === 'resolved' ? state.installError : null;
		const files = r.files.length
			? `${r.files.length} ${plural(r.files.length, 'file')} · ${r.files.slice(0, 4).join(', ')}${r.files.length > 4 ? ', …' : ''}`
			: 'no files listed';
		body = (
			<>
				<div className="subhead first">Resolved</div>
				<div className="drow" data-resolved-source>
					<span className="k2">Source</span>
					<span className="val mono">
						{r.source} · {shortSource(r.url)}
					</span>
					<span className="rt">
						<span className="meta">public</span>
						{state.step === 'resolved' && (
							<button
								type="button"
								className="btn ghost"
								data-edit
								style={{ height: 20, padding: '0 6px' }}
								onClick={() => setState({ step: 'empty' })}
							>
								Edit
							</button>
						)}
					</span>
				</div>
				<div className="drow">
					<span className="k2">Kind</span>
					<span className="val">{r.kind}</span>
					<span className="rt meta">
						{kind === 'infer' ? 'inferred · ' : ''}
						{r.inferredFrom}
					</span>
				</div>
				<div className="drow">
					<span className="k2">Version</span>
					<span className="val mono">{pinText}</span>
					<span className="rt meta">
						{r.source === 'npx'
							? 'as npx served it'
							: r.ref
								? `${r.ref} · pinned to this commit`
								: 'HEAD of the default branch · pinned to this commit'}
					</span>
				</div>
				<div className="drow">
					<span className="k2">Files</span>
					<span className="val">{files}</span>
				</div>

				<div className="subhead">Requires — the closure, before you consent</div>
				{closure.length ? (
					<ClosureRows deps={closure} />
				) : (
					<div className="drow">
						<span className="k2">Closure</span>
						<span className="val no">nothing — this installs alone</span>
					</div>
				)}

				<div className="subhead">Trust</div>
				<div className="trustbox" data-unsigned-warning>
					<AlertTriangle className="ico h-3.5 w-3.5" />
					<span>
						<span className="t1">Unsigned — review it before you install</span>
						<span className="t2">
							Nothing vouches for this source. It is not in the signed catalog, and its files carry
							no signature. A {r.kind} cannot grant itself permissions, but every session that loads
							it follows its instructions with the tools you have already allowed.
						</span>
					</span>
				</div>

				<div className="subhead">Share kola</div>
				<Consent
					id="src"
					b={shortSource(r.url)}
					d={`I have read it, or I trust whoever published it. Pinned to ${pinText} — what I install is what was resolved.`}
					checked={Boolean(ticked.src) || installing}
					disabled={installing}
					onChange={(v) => setTicked((t) => ({ ...t, src: v }))}
				/>
				{extra.map((d) => (
					<Consent
						key={d.name}
						id={`dep:${d.name}`}
						b={d.name}
						d={`also comes from outside the catalog (${depProvenance(d)}).`}
						checked={Boolean(ticked[`dep:${d.name}`]) || installing}
						disabled={installing}
						onChange={(v) => setTicked((t) => ({ ...t, [`dep:${d.name}`]: v }))}
					/>
				))}
				<p className="consentnote">
					A direct install never updates itself. Update re-fetches only when you ask, and shows the
					new version first.
				</p>
			</>
		);
		const scopeFor = (s: StoreInstallScope) => (s === 'personal' ? 'personal' : projectLabel);
		foot = (
			<>
				{installing && (
					<span className="emberbar" role="status" data-stage>
						<i />{' '}
						{state.stage === 'fetch'
							? `Fetching ${pinText}${fetchN > 1 ? ' → Materializing deps' : ''}`
							: `Placing in ${scopeFor(state.scope)}`}
					</span>
				)}
				{installError && (
					<span className="note bad" role="alert" data-action-error>
						Failed: {installError}
					</span>
				)}
				<InstallSplit
					projectLabel={projectLabel}
					blocked={
						!onInstall
							? (disabledReason ?? NOT_AVAILABLE_ON_SERVER_YET)
							: allTicked
								? null
								: consentBlockedReason(
										consentIds.filter((id) => ticked[id]).length,
										consentIds.length,
										'Tick every box under Share kola first'
									)
					}
					busy={installing}
					onInstall={(s) => void install(r, s)}
				/>
				<span className="note" style={{ marginLeft: 'auto' }}>
					fetches {fetchN}
				</span>
			</>
		);
	}

	return (
		<>
			<div className="dhead">
				<div className="dtitle">
					<span className="ico">
						<Link2 className="h-4 w-4" />
					</span>
					{header}
					<span style={{ flex: 1 }} />
					<button type="button" className="iconbtn" aria-label="Close sheet" onClick={onClose}>
						<X className="h-3.5 w-3.5" />
					</button>
				</div>
				<div className="dsub">
					<span>a skill, agent or command straight from its source</span>
					<span>nothing here has been reviewed</span>
				</div>
			</div>
			<div className="sheetbody sc" data-addurl-sheet data-state={state.step}>
				{body}
			</div>
			<div className="sheetfoot" data-sheetfoot>
				{foot}
			</div>
		</>
	);
}
