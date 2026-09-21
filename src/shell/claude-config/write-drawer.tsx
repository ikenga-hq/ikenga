// Ngwa v2b — D-09 multi-destination transcode copy drawer (WP-26).
//
// The locked design is `plans/cockpit/designs/write-transcode-drawer.html`. The
// drawer presents every (engine × scope) destination as a checkbox row tagged
// with its per-destination transcode mode (`md→md · same` / `md→toml ·
// transcode`); the source row is disabled; blocked reverse destinations
// (TOML→MD — no reverse transcoder, see `06-cross-engine-transcode.md`) render
// greyed with a tooltip. A live summary counts selected · copy + transcode and
// flags blocked dirs; a transcode preview renders for the `md→toml`
// destinations. The batch "Copy to N" fires `useCrossEngineCopy` and renders
// per-row partial-failure results.
//
// Cross-engine copy is FILE-BASED kinds only (skill | agent | command) per the
// directionality matrix; hooks/mcp are settings-embedded and out of v2b's
// cross-engine scope (they use the per-engine enable/disable path instead).
//
// MOCK SEAM: the batch-copy backend (WP-24) is not built — `useCrossEngineCopy`
// is mock-backed (tauri-cmd `NGWA_TRANSCODE_MOCK`) and returns a canned per-row
// result. The drawer is finalized against WP-24's real command later; the wire
// shape (`NgwaCopyDestination` / `NgwaCopyBatchResult`) is the frozen contract.

import { useMemo, useRef, useState } from 'react';

import { cn } from '@/components/ui/utils';
import { useFocusTrap } from '@/lib/a11y/focus';
import type { ClaudeAgent, ClaudeCommand, ClaudeSkill, ClaudeStoreScope } from '@/lib/tauri-cmd';
import {
	useCrossEngineCopy,
	type ClaudeStoreKind,
	type ConfigFormat,
	type EngineId,
	type NgwaCopyBatchResult,
	type NgwaCopyDestination,
	type NgwaStoreError,
	type NgwaTranscodeMode,
} from '@/lib/queries/claude-config';

import { ENGINE_META, ENGINE_ORDER, type EngineConfigItem, type NgwaSystemId } from './ngwa-surface';
import { EngineGlyph } from './engine-glyph';

// ─── Directionality matrix (06-cross-engine-transcode.md) ────────────────────
//
// Per-(engine, kind) on-disk format for the file-based kinds. Drives both the
// per-row transcode cue and the blocked-reverse greying. Mirrors the frozen
// `EngineLayout`: Claude/Gemini agents+skills are MD+YAML; Codex agents are
// TOML; Claude commands are MD, Gemini/Codex commands are TOML; skills are MD
// under every engine.
const ENGINE_KIND_FORMAT: Record<EngineId, Partial<Record<ClaudeStoreKind, ConfigFormat>>> = {
	claude: { agent: 'md-yaml', skill: 'md-yaml', command: 'md-yaml' },
	gemini: { agent: 'md-yaml', skill: 'md-yaml', command: 'toml' },
	codex: { agent: 'toml', skill: 'md-yaml', command: 'toml' },
};

/** File-based kinds are the only ones cross-engine copy supports in v2b. */
const FILE_BASED: ReadonlySet<ClaudeStoreKind> = new Set<ClaudeStoreKind>([
	'skill',
	'agent',
	'command',
]);

export function isCrossEngineCopyable(kind: ClaudeStoreKind): boolean {
	return FILE_BASED.has(kind);
}

/** Resolve the transcode relationship for copying `kind` from `fromEngine` to
 *  `toEngine`, per the directionality matrix:
 *    • same format → `same` (verbatim copy, no transcode)
 *    • MD → TOML   → `transcode` (forward, via the existing transcoder)
 *    • TOML → MD   → `blocked` (no reverse transcoder)
 *  An unknown (engine, kind) pair resolves to `blocked` (refuse rather than
 *  guess). */
export function resolveTranscodeMode(
	kind: ClaudeStoreKind,
	fromEngine: EngineId,
	toEngine: EngineId
): NgwaTranscodeMode {
	const from = ENGINE_KIND_FORMAT[fromEngine]?.[kind];
	const to = ENGINE_KIND_FORMAT[toEngine]?.[kind];
	if (!from || !to) return 'blocked';
	if (from === to) return 'same';
	if (from === 'md-yaml' && to === 'toml') return 'transcode';
	// toml → md-yaml (the only remaining cross pair) — no reverse transcoder.
	return 'blocked';
}

const FMT_OF: Record<ConfigFormat, string> = {
	'md-yaml': 'md',
	toml: 'toml',
	'json-embedded': 'json',
};

const MODE_LABEL: Record<NgwaTranscodeMode, (from: string, to: string) => string> = {
	same: (f) => `${f} → ${f} · same`,
	transcode: (f, t) => `${f} → ${t} · transcode`,
	blocked: (f, t) => `${t} → ${f} · blocked`,
};

const BLOCKED_TOOLTIP =
	'No reverse transcoder yet — copying a TOML primitive back to Markdown is not supported (see 06-cross-engine-transcode.md).';

// ─── Destination model ───────────────────────────────────────────────────────

interface DestRow {
	key: string;
	/** Engine sent to the backend. For a merged cross-tool skill row this is the
	 *  canonical engine (Gemini/Codex resolve to the SAME `.agents/skills/` path,
	 *  so either writes the one symlink). */
	engine: NgwaSystemId;
	/** Engines this row covers. >1 only for the merged cross-tool skill row
	 *  (Gemini + Codex). Drives the logo(s) + label. */
	coversEngines: NgwaSystemId[];
	scope: ClaudeStoreScope;
	scopeLabel: string;
	mode: NgwaTranscodeMode;
	/** True for the source's own slot — disabled, shown as `source`. */
	isSource: boolean;
}

/** Enumerate destinations for the source item, grouped by engine then scope.
 *  Skills are special-cased: Gemini + Codex skills share the cross-tool
 *  `.agents/skills/<name>` path, so they MERGE into a single row per scope
 *  (separate rows would write the same symlink twice). Agents/commands keep
 *  distinct per-engine paths → one row each. Mode is resolved per the matrix. */
function buildDestinations(
	item: EngineConfigItem,
	present: readonly NgwaSystemId[],
	projectScopes: Array<{ key: ClaudeStoreScope; label: string }>
): DestRow[] {
	const scopes: Array<{ key: ClaudeStoreScope; label: string }> = [
		{ key: 'workspace', label: 'user' },
		...projectScopes,
	];
	const rows: DestRow[] = [];
	const isSkill = item.storeKind === 'skill';
	// Engines that share the cross-tool `.agents/skills/` path (skills only).
	const crossTool = ENGINE_ORDER.filter(
		(e) => present.includes(e) && (e === 'gemini' || e === 'codex')
	);

	const pushPerEngine = (engine: NgwaSystemId) => {
		for (const s of scopes) {
			const isSource = engine === item.system && s.key === item.scopeKey;
			rows.push({
				key: `${engine}:${s.key}`,
				engine,
				coversEngines: [engine],
				scope: s.key,
				scopeLabel: s.label,
				mode: isSource ? 'same' : resolveTranscodeMode(item.storeKind, item.system, engine),
				isSource,
			});
		}
	};

	if (isSkill) {
		// Claude keeps its own `.claude/skills/` path.
		if (present.includes('claude')) pushPerEngine('claude');
		// Gemini + Codex collapse into one cross-tool `.agents/skills/` row/scope.
		if (crossTool.length > 0) {
			for (const s of scopes) {
				const isSource = crossTool.includes(item.system) && s.key === item.scopeKey;
				rows.push({
					key: `agents:${s.key}`,
					engine: crossTool[0],
					coversEngines: crossTool,
					scope: s.key,
					scopeLabel: s.label,
					mode: 'same', // skill copies are always same-format (md→md → symlink)
					isSource,
				});
			}
		}
	} else {
		for (const engine of ENGINE_ORDER.filter((e) => present.includes(e))) {
			pushPerEngine(engine);
		}
	}
	return rows;
}

// ─── Transcode preview (md → toml) ───────────────────────────────────────────
//
// A faithful-enough preview of the forward transcode: frontmatter keys map 1:1
// to TOML key/value lines, the body becomes a `system_prompt` (agents/skills) /
// `prompt` (commands) triple-quoted block. This is a UI preview only — the
// authoritative transcode runs in `transcoder.rs` (WP-24); we never write here.

function sourceBodyOf(item: EngineConfigItem): { frontmatter: Record<string, unknown>; body: string } {
	if (item.storeKind === 'agent') {
		const a = item.raw as ClaudeAgent;
		return { frontmatter: a.frontmatter ?? {}, body: a.body ?? '' };
	}
	if (item.storeKind === 'skill') {
		const s = item.raw as ClaudeSkill;
		return { frontmatter: s.frontmatter ?? {}, body: s.body ?? '' };
	}
	if (item.storeKind === 'command') {
		const c = item.raw as ClaudeCommand;
		return { frontmatter: c.frontmatter ?? {}, body: c.body ?? '' };
	}
	return { frontmatter: {}, body: '' };
}

function tomlValue(v: unknown): string {
	if (typeof v === 'string') return JSON.stringify(v);
	if (typeof v === 'number' || typeof v === 'boolean') return String(v);
	if (Array.isArray(v)) return `[${v.map((x) => tomlValue(x)).join(', ')}]`;
	return JSON.stringify(String(v));
}

function renderTranscodePreview(item: EngineConfigItem): { srcText: string; dstLines: React.ReactNode[] } {
	const { frontmatter, body } = sourceBodyOf(item);
	const promptKey = item.storeKind === 'command' ? 'prompt' : 'system_prompt';
	const fmEntries = Object.entries(frontmatter);

	// Source pane: reconstruct the MD+YAML shape from the parsed frontmatter +
	// body (we don't have raw bytes here; the parsed form is faithful enough for
	// a preview).
	const srcFm = fmEntries.map(([k, v]) => `${k}: ${Array.isArray(v) ? `[${v.join(', ')}]` : v}`);
	const srcText = `---\n${srcFm.join('\n')}\n---\n${body.trim().split('\n').slice(0, 4).join('\n')}`;

	const dstLines: React.ReactNode[] = [];
	for (const [k, v] of fmEntries) {
		dstLines.push(
			<div key={`fm-${k}`}>
				<span className="key">{k}</span> = {tomlValue(v)}
			</div>
		);
	}
	const bodyPreview = body.trim().split('\n').slice(0, 4).join('\n');
	dstLines.push(
		<div key="prompt" className="add">
			{promptKey} = """
			{'\n'}
			{bodyPreview}
			{'\n'}
			"""
		</div>
	);
	return { srcText, dstLines };
}

// ─── The drawer ──────────────────────────────────────────────────────────────

export function WriteTranscodeDrawer({
	item,
	present,
	projectScopes,
	onClose,
}: {
	item: EngineConfigItem;
	present: readonly NgwaSystemId[];
	projectScopes: Array<{ key: ClaudeStoreScope; label: string }>;
	onClose: () => void;
}) {
	const copy = useCrossEngineCopy();
	const [result, setResult] = useState<NgwaCopyBatchResult | null>(null);

	// Modal dialog semantics (WCAG 4.1.2 / 2.4.3): trap Tab inside the drawer
	// while open and return focus to the trigger on close. Esc-to-close is wired
	// on the aside below (the trap stays escapable — WCAG 2.1.2).
	const drawerRef = useRef<HTMLElement | null>(null);
	useFocusTrap(drawerRef, { enabled: true });

	const dests = useMemo(
		() => buildDestinations(item, present, projectScopes),
		[item, present, projectScopes]
	);

	// Selectable = not source + not blocked. Default: nothing selected.
	const [selected, setSelected] = useState<Set<string>>(new Set());

	function toggle(row: DestRow) {
		if (row.isSource || row.mode === 'blocked') return;
		setSelected((prev) => {
			const next = new Set(prev);
			next.has(row.key) ? next.delete(row.key) : next.add(row.key);
			return next;
		});
		// Picking a new set invalidates the last batch's results.
		setResult(null);
	}

	const selectedRows = dests.filter((d) => selected.has(d.key));
	const copyCount = selectedRows.filter((d) => d.mode === 'same').length;
	const transcodeCount = selectedRows.filter((d) => d.mode === 'transcode').length;
	const hasBlocked = dests.some((d) => d.mode === 'blocked' && !d.isSource);
	const anyTranscodeSelected = transcodeCount > 0;

	const preview = useMemo(
		() => (anyTranscodeSelected ? renderTranscodePreview(item) : null),
		[anyTranscodeSelected, item]
	);

	function submit() {
		if (selectedRows.length === 0) return;
		const destinations: NgwaCopyDestination[] = selectedRows.map((d) => ({
			engine: d.engine,
			scope: d.scope,
			mode: d.mode,
		}));
		copy.mutate(
			{
				fromEngine: item.system,
				kind: item.storeKind,
				name: item.name,
				fromScope: item.scopeKey,
				destinations,
			},
			{ onSuccess: (res) => setResult(res) }
		);
	}

	const resultByKey = useMemo(() => {
		const m = new Map<string, NgwaCopyBatchResult['rows'][number]>();
		if (result) for (const r of result.rows) m.set(`${r.engine}:${r.scope}`, r);
		return m;
	}, [result]);

	const transcodeFnLabel =
		item.storeKind === 'command' ? 'md_to_gemini_command_toml' : 'md_to_codex_toml';

	return (
		<>
			{/* biome-ignore lint/a11y/noStaticElementInteractions: mouse-dismiss backdrop; the keyboard path is the dialog's Esc handler + × button + focus-trap below. */}
			{/* biome-ignore lint/a11y/useKeyWithClickEvents: same — backdrop click is a sighted-mouse convenience, not a keyboard control. */}
			<div className="ngwa-dr-scrim show" onClick={onClose} />
			<aside
				ref={drawerRef}
				className="ngwa-dr show"
				role="dialog"
				aria-modal="true"
				aria-labelledby="ngwa-dr-title"
				onKeyDown={(e) => {
					if (e.key === 'Escape') {
						e.stopPropagation();
						onClose();
					}
				}}
			>
				<div className="ngwa-dr-head">
					<h3 id="ngwa-dr-title">Copy “{item.name}”</h3>
					<button type="button" className="x" onClick={onClose} aria-label="Close">
						✕
					</button>
				</div>

				<div className="ngwa-dr-body">
					<div className="ngwa-dr-sect">
						<div className="ngwa-dr-label">Destinations — pick any</div>
						<div className="ngwa-dlist">
							{/* WCAG 1.3.1 — a persistent, AT-reachable description for the
							    blocked (TOML→Markdown) rows, referenced via aria-describedby
							    instead of a pointer-only `title`. */}
							<span id="ngwa-blocked-desc" className="sr-only">
								{BLOCKED_TOOLTIP}
							</span>
							{dests.map((d) => {
								const on = selected.has(d.key);
								const blocked = d.mode === 'blocked';
								const rowRes = resultByKey.get(d.key);
								return (
									<label
										key={d.key}
										className={cn(
											'ngwa-drow',
											on && 'on',
											d.isSource && 'src',
											blocked && 'blocked'
										)}
										aria-describedby={blocked ? 'ngwa-blocked-desc' : undefined}
										onClick={(e) => {
											e.preventDefault();
											toggle(d);
										}}
									>
										<span className="cb">{d.isSource ? '–' : blocked ? '∅' : on ? '✓' : ''}</span>
										<span className="de">
											{d.coversEngines.map((e) => (
												<span key={e} className={cn('eg', ENGINE_META[e].code)} aria-hidden>
													<EngineGlyph system={e} />
												</span>
											))}
											{d.coversEngines.map((e) => ENGINE_META[e].display).join(' + ')} ·{' '}
											{d.scopeLabel}
										</span>
										{rowRes ? (
											<span className={cn('rowres', rowRes.ok ? 'ok' : 'fail')}>
												{rowRes.ok ? '✓ written' : '✕ failed'}
											</span>
										) : (
											<span
												className={cn(
													'xf',
													d.isSource && 'source',
													!d.isSource && d.mode === 'same' && 'same',
													!d.isSource && d.mode === 'transcode' && 'tc',
													!d.isSource && blocked && 'bl'
												)}
											>
												{d.isSource
													? 'source'
													: MODE_LABEL[d.mode](
															FMT_OF[
																ENGINE_KIND_FORMAT[item.system]?.[item.storeKind] ?? 'md-yaml'
															],
															FMT_OF[ENGINE_KIND_FORMAT[d.engine]?.[item.storeKind] ?? 'toml']
														)}
											</span>
										)}
									</label>
								);
							})}
						</div>
						<div className="ngwa-dsum">
							<b>{selectedRows.length} selected</b>
							{selectedRows.length > 0 && (
								<>
									{' · '}
									{copyCount} copy + {transcodeCount} transcode
								</>
							)}
							{hasBlocked && (
								<>
									{' · '}
									<span className="bl">reverse (TOML→Markdown) blocked</span>
								</>
							)}
						</div>
					</div>

					{preview && (
						<div className="ngwa-dr-sect">
							<div className="ngwa-dr-label">
								Transcode preview · applies to the{' '}
								<b style={{ color: 'var(--fmt-toml)' }}>md→toml</b> destinations (
								<code className="ngwa-tcfn">{transcodeFnLabel}</code>)
							</div>
							<div className="ngwa-diff">
								<div className="ngwa-pane src">
									<div className="ph">
										<span>source</span>
										<span className="f">
											{item.name}.
											{FMT_OF[ENGINE_KIND_FORMAT[item.system]?.[item.storeKind] ?? 'md-yaml']}
										</span>
									</div>
									<div className="code">{preview.srcText}</div>
								</div>
								<div className="ngwa-pane dst">
									<div className="ph">
										<span>will write</span>
										<span className="f">{item.name}.toml</span>
									</div>
									<div className="code">{preview.dstLines}</div>
								</div>
							</div>
						</div>
					)}

					<div className="ngwa-dr-warn">
						<b>Faithful transcode.</b> Each transcode destination maps frontmatter keys 1:1 and body
						→ <code>{item.storeKind === 'command' ? 'prompt' : 'system_prompt'}</code>, no fields
						dropped. Same-format destinations copy verbatim — no transcode. Any{' '}
						<b>TOML → Markdown</b> reverse is blocked (no reverse transcoder).
					</div>

					{copy.isError && (
						<div className="ngwa-dr-warn err">
							<b>Batch failed.</b> {String(copy.error)}
						</div>
					)}
					{result && (
						<div className="ngwa-dr-sect">
							<div className="ngwa-dr-label">
								Result · {result.rows.filter((r) => r.ok).length}/{result.rows.length} written
							</div>
							<div className="ngwa-dlist">
								{result.rows.map((r) => (
									<div key={`${r.engine}:${r.scope}`} className="ngwa-drow res">
										<span className="de">
											<span className={cn('eg', ENGINE_META[r.engine].code)} aria-hidden>
												<EngineGlyph system={r.engine} />
											</span>
											{ENGINE_META[r.engine].display} ·{' '}
											{r.scope === 'workspace' ? 'user' : r.scope.replace('project:', '')}
										</span>
										{r.ok ? (
											<span className="rowres ok" title={r.mutation.path}>
												✓ {r.mode === 'transcode' ? 'transcoded' : 'copied'}
											</span>
										) : (
											<span className="rowres fail" title={errorDetail(r.error)}>
												✕ {r.error.kind}
											</span>
										)}
									</div>
								))}
							</div>
						</div>
					)}
				</div>

				<div className="ngwa-dr-foot">
					<span className="note">
						writes {selectedRows.length} {selectedRows.length === 1 ? 'file' : 'files'} · atomic per
						destination · per-row results
					</span>
					<button type="button" className="ngwa-btn" onClick={onClose}>
						{result ? 'Done' : 'Cancel'}
					</button>
					<button
						type="button"
						className="ngwa-btn primary"
						disabled={selectedRows.length === 0 || copy.isPending}
						onClick={submit}
					>
						{copy.isPending ? 'Copying…' : `Copy to ${selectedRows.length} →`}
					</button>
				</div>
			</aside>
		</>
	);
}

function errorDetail(e: NgwaStoreError): string {
	switch (e.kind) {
		case 'strictKeyRejected':
			return `strict ${e.engine} settings.json would reject key "${e.key}"`;
		case 'nonTableParent':
			return `${e.path}: parent "${e.key}" is not a table`;
		case 'parse':
		case 'unrepresentableValue':
		case 'io':
			return `${e.path}: ${e.message}`;
		case 'unsupported':
			return e.message;
		default:
			return '';
	}
}
