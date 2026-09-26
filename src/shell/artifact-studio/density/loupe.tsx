// Loupe density — single-artifact view.
//
// Layout: chrome / [renderer | right-rail] / version-strip.
//
// Right rail is tabbed (Terminal / Code / DOM / Manifest), default Terminal.
// The Code, DOM, and Manifest tabs are only meaningful for a single
// focused artifact, so they live on this density only (grid and compare
// have no right rail per the unified plan).

import { type ArtifactManifest, ArtifactManifestSchema } from '@ikenga/contract/artifact';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import {
	FolderTree,
	Pencil,
	Pin as PinGlyph,
	RefreshCw,
	Save,
	Settings as SinkIcon,
	SquareDashedMousePointer,
	TreePine,
	X,
} from 'lucide-react';
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import * as M from '@/lib/artifact/bridge-messages';
import { wrapHostMessage } from '@/lib/artifact/bridge-messages';
import { Panel, PanelGroup, PanelResizeHandle } from 'react-resizable-panels';
import { IconButton } from '@/components/ui/icon-button';
import { cn } from '@/components/ui/utils';
import { useFocusTrap } from '@/lib/a11y/focus';
import { focusMarkerProps } from '@/lib/keymap/context-keys';
import { useCommands } from '@/lib/keymap/dispatcher';
import { extractManifestJson } from '@/lib/artifact/manifest-from-file';
import { writeManifestIntoHtml } from '@/lib/artifact/manifest-write';
import { routeOutcomeLabel, routePin } from '@/lib/artifact/route-pin';
import { usePaneStore } from '@/lib/panes/pane-store';
import { useRecordRecentArtifact } from '@/lib/shell/artifact-grid-recent-artifacts';
import {
	type Comment,
	commentList,
	commentSetStatus,
	fsListenWatch,
	fsRead,
	fsUnwatch,
	fsWatch,
	fsWrite,
	type IykeDomResult,
	iykeDomQuery,
} from '@/lib/tauri-cmd';
import { type PickResult, PinComposer } from '@/shell/artifact-studio/pin-composer';
import { pickRenderer } from '@/shell/artifact-studio/renderers';
import { RightRail, useRightRailTab } from '@/shell/artifact-studio/right-rail';
import { StudioCommentMode } from '@/shell/artifact-studio/studio-comment-mode';
import { StudioManifestEditor } from '@/shell/artifact-studio/studio-manifest-editor';
import { StudioPromoteDialog } from '@/shell/artifact-studio/studio-promote-dialog';
import {
	type StudioSink,
	StudioSinkPopover,
	studioSinkToPreferredPtyId,
	studioSinkToRouteOverride,
	useArtifactSink,
} from '@/shell/artifact-studio/studio-sink-popover';
import { StudioSourceEditor } from '@/shell/artifact-studio/studio-source-editor';
import {
	StudioTerminal,
	StudioTerminalAttachButton,
	TerminalChip,
} from '@/shell/artifact-studio/studio-terminal';
import { StudioTextEditMode } from '@/shell/artifact-studio/studio-text-edit-mode';
import { VersionStrip } from '@/shell/artifact-studio/version-strip';
import { useTerminalStore } from '@/terminal/session-store';

interface StudioLoupeProps {
	path: string;
	paneId: string;
	attachedTerminalId?: string;
}

export function StudioLoupe({ path, paneId, attachedTerminalId }: StudioLoupeProps) {
	// Record each loupe open against the active project's recent-artifacts
	// list. No-op when path is empty or no project is active. Fire-and-forget.
	useRecordRecentArtifact(path);

	const [source, setSource] = useState<string | null>(null);
	const [savedSource, setSavedSource] = useState<string | null>(null);
	const [loadError, setLoadError] = useState<string | null>(null);
	const [commentMode, setCommentMode] = useState(false);
	const [textEditMode, setTextEditMode] = useState(false);
	const [promoteOpen, setPromoteOpen] = useState(false);
	const [sinkOpen, setSinkOpen] = useState(false);
	const [pendingPick, setPendingPick] = useState<PickResult | null>(null);
	// Single agent slot — the terminal tab renders the embedded PTY (or a
	// picker when no terminal is attached). Always start on the agent slot.
	const [rightTab, setRightTab] = useRightRailTab('terminal');
	const { sink, setSink } = useArtifactSink(path);

	const onAttachTerminal = useCallback(
		(tabId: string) => {
			usePaneStore.getState().setStudioAttachedTerminal(paneId, tabId);
		},
		[paneId]
	);
	const onDetachTerminal = useCallback(() => {
		if (attachedTerminalId) {
			useTerminalStore.getState().detachFromStudio(attachedTerminalId);
		}
		usePaneStore.getState().setStudioAttachedTerminal(paneId, null);
	}, [paneId, attachedTerminalId]);

	const attachedTabTitle = useTerminalStore((s) =>
		attachedTerminalId ? s.tabs.find((t) => t.id === attachedTerminalId)?.title : undefined
	);
	const attachmentOverride = useMemo(
		() =>
			attachedTerminalId
				? { tabId: attachedTerminalId, label: attachedTabTitle ?? 'terminal' }
				: null,
		[attachedTerminalId, attachedTabTitle]
	);

	useEffect(() => {
		let cancelled = false;
		(async () => {
			try {
				const result = await fsRead(path);
				const html = new TextDecoder('utf-8', { fatal: false }).decode(
					new Uint8Array(result.bytes)
				);
				if (!cancelled) {
					setSource(html);
					setSavedSource(html);
				}
			} catch (e) {
				if (!cancelled) setLoadError(e instanceof Error ? e.message : String(e));
			}
		})();
		return () => {
			cancelled = true;
		};
	}, [path]);

	const dirty = source !== null && savedSource !== null && source !== savedSource;

	const manifest = useMemo<ArtifactManifest | null>(() => {
		if (source === null) return null;
		const json = extractManifestJson(source);
		if (!json) return null;
		try {
			const parsed = JSON.parse(json);
			const validated = ArtifactManifestSchema.safeParse(parsed);
			return validated.success ? validated.data : (parsed as ArtifactManifest);
		} catch {
			return null;
		}
	}, [source]);

	const save = useCallback(async () => {
		if (source === null || !dirty) return;
		try {
			await fsWrite(path, new TextEncoder().encode(source));
			setSavedSource(source);
		} catch (e) {
			setLoadError(e instanceof Error ? e.message : String(e));
		}
	}, [path, source, dirty]);

	const applyEngineEdit = useCallback(
		async (next: string) => {
			setSource(next);
			try {
				await fsWrite(path, new TextEncoder().encode(next));
				setSavedSource(next);
			} catch (e) {
				setLoadError(e instanceof Error ? e.message : String(e));
			}
		},
		[path]
	);

	// WP-56 (G-ACTIONS §10.2/§10.6): migrated from a local ⌘S `onKeyDown` to
	// the registry `studio.loupe-save` command, scoped by the new `loupeFocus`
	// key (B-21) marked on the pane root below.
	useCommands({ 'studio.loupe-save': () => void save() });

	const updateManifest = useCallback(
		(next: ArtifactManifest, opts: { save?: boolean } = {}) => {
			if (source === null) return;
			const nextSource = writeManifestIntoHtml(source, next);
			setSource(nextSource);
			if (opts.save) {
				void fsWrite(path, new TextEncoder().encode(nextSource)).then(() =>
					setSavedSource(nextSource)
				);
			}
		},
		[path, source]
	);

	// Renderer kind hint deferred — today's ArtifactManifest schema is
	// HTML-flavoured (no `kind` field). When the schema grows a discriminator
	// (open question 1 in the unified plan), thread it in here.
	const Renderer = useMemo(() => pickRenderer(path).Component, [path]);

	if (loadError) {
		return (
			<div className="flex h-full w-full items-center justify-center p-6 text-sm text-destructive">
				Failed to open artifact: {loadError}
			</div>
		);
	}
	if (source === null) {
		return (
			<div className="flex h-full w-full items-center justify-center p-6 text-sm text-muted-foreground">
				Loading…
			</div>
		);
	}

	return (
		<div
			className="flex h-full w-full flex-col bg-background"
			{...focusMarkerProps('loupe')}
			data-pane-id={paneId}
			role="application"
			aria-label="Artifact Studio"
		>
			<StudioChrome
				path={path}
				dirty={dirty}
				manifest={manifest}
				commentMode={commentMode}
				textEditMode={textEditMode}
				sink={sink}
				paneId={paneId}
				artifactPath={path}
				attachedTerminalId={attachedTerminalId}
				onAttachTerminal={onAttachTerminal}
				onShowTerminalTab={() => setRightTab('terminal')}
				onDetachTerminal={onDetachTerminal}
				onCommentModeToggle={() => {
					setCommentMode((v) => !v);
					// Comment + text-edit modes are mutually exclusive — they both
					// claim the iframe doc's click listener.
					setTextEditMode(false);
				}}
				onTextEditModeToggle={() => {
					setTextEditMode((v) => !v);
					setCommentMode(false);
				}}
				onSinkOpen={() => setSinkOpen(true)}
				onSave={save}
				onPromote={() => setPromoteOpen(true)}
				onClose={() =>
					usePaneStore.getState().replaceActiveViewAndPushHistory(paneId, {
						kind: 'artifact',
						path,
					})
				}
				onPinToggle={() => {
					if (!manifest) return;
					updateManifest(
						{
							...manifest,
							pin: {
								...(manifest.pin ?? { suggested: false }),
								suggested: !manifest.pin?.suggested,
							},
						},
						{ save: true }
					);
				}}
			/>
			<div className="flex-1 min-h-0 overflow-hidden">
				<PanelGroup direction="horizontal" autoSaveId={`studio-loupe:${path}`}>
					<Panel defaultSize={70} minSize={30}>
						<div className="relative h-full w-full">
							<Renderer path={path} paneId={paneId} density="loupe" source="pane" />
							<LoupePinOverlay
								path={path}
								paneId={paneId}
								sink={sink}
								attachedTerminalId={attachedTerminalId}
							/>
							{commentMode && <StudioCommentMode paneId={paneId} onPick={setPendingPick} />}
							{textEditMode && source !== null && (
								<StudioTextEditMode
									paneId={paneId}
									source={source}
									onCommit={(nextSource) => void applyEngineEdit(nextSource)}
								/>
							)}
						</div>
					</Panel>
					<PanelResizeHandle className="w-px bg-border hover:bg-accent" />
					<Panel defaultSize={30} minSize={20}>
						<RightRail
							tab={rightTab}
							onChangeTab={setRightTab}
							slots={{
								// Terminal slot — embedded PTY or picker.
								terminal: (
									<StudioTerminal
										paneId={paneId}
										artifactPath={path}
										attachedTerminalId={attachedTerminalId ?? null}
										onAttach={onAttachTerminal}
										onDetach={onDetachTerminal}
									/>
								),
								code: <StudioSourceEditor value={source} onChange={setSource} />,
								dom: <DomInspector paneId={paneId} path={path} />,
								manifest: (
									<StudioManifestEditor
										manifest={manifest}
										onChange={(next) => updateManifest(next)}
									/>
								),
							}}
						/>
					</Panel>
				</PanelGroup>
			</div>
			<VersionStrip paneId={paneId} path={path} />
			<StudioPromoteDialog
				open={promoteOpen}
				onOpenChange={setPromoteOpen}
				path={path}
				source={source}
				manifest={manifest}
			/>
			<PinComposer
				open={pendingPick !== null}
				pick={pendingPick}
				artifactPath={path}
				attachedTerminalId={attachedTerminalId}
				onClose={(committed) => {
					setPendingPick(null);
					// Drop comment-mode after a successful pin so the user isn't
					// stuck in capture-mode. On cancel/dismiss, leave it armed
					// — they may have mis-clicked and want another try.
					if (committed) setCommentMode(false);
				}}
			/>
			<StudioSinkPopover
				open={sinkOpen}
				onOpenChange={setSinkOpen}
				anchorEl={
					sinkOpen
						? (document.querySelector<HTMLElement>(
								`[data-pane-id="${paneId}"] [data-studio-sink-anchor]`
							) ?? null)
						: null
				}
				sink={sink}
				onSinkChange={(next) => void setSink(next)}
				attachmentOverride={attachmentOverride}
			/>
		</div>
	);
}

// ─── Pin overlay ─────────────────────────────────────────────────────
//
// Renders open + in-progress pins over the renderer panel. Each pin's
// live rect is resolved by `iframe.contentDocument.querySelector(selector)`
// (same-origin: the viewer-server serves the artifact, the picker already
// uses this path). Re-projects on iframe content scroll, host resize, and
// post-edit DOM mutations.
//
// Pins whose selector no longer matches surface as a stale strip on the
// right edge — they remain reachable for review/resolve even when the
// element they pointed at has been refactored away. Resolved pins are
// hidden; loupe is the "focused work" surface.

interface ResolvedPin {
	pin: Comment;
	numbering: number;
	rect: { x: number; y: number } | null; // null = stale
}

/** Spread pins whose dots would otherwise stack on the same coordinates
 *  (i.e. multiple pins targeting the same DOM element). Without this the
 *  top dot eats all clicks and the underlying pins are unreachable. The
 *  first pin in a cluster keeps the centre; the rest fan out clockwise
 *  on a small ring (12px radius). */
function spreadOverlaps(pins: ResolvedPin[]): ResolvedPin[] {
	const RADIUS = 12;
	const TOL = 2; // px tolerance when bucketing
	const buckets = new Map<string, ResolvedPin[]>();
	const order: string[] = [];
	for (const p of pins) {
		if (!p.rect) continue;
		const key = `${Math.round(p.rect.x / TOL)},${Math.round(p.rect.y / TOL)}`;
		if (!buckets.has(key)) {
			buckets.set(key, []);
			order.push(key);
		}
		buckets.get(key)!.push(p);
	}
	const offsetByPinId = new Map<number, { dx: number; dy: number }>();
	for (const key of order) {
		const cluster = buckets.get(key)!;
		if (cluster.length < 2) continue;
		// Distribute around a circle, starting at angle 0 (right). The first
		// pin stays at centre; siblings sit at evenly-spaced angles.
		const siblings = cluster.length - 1;
		for (let i = 1; i < cluster.length; i++) {
			const angle = ((i - 1) / siblings) * Math.PI * 2;
			offsetByPinId.set(cluster[i].pin.id, {
				dx: Math.cos(angle) * RADIUS,
				dy: Math.sin(angle) * RADIUS,
			});
		}
	}
	if (offsetByPinId.size === 0) return pins;
	return pins.map((p) => {
		if (!p.rect) return p;
		const off = offsetByPinId.get(p.pin.id);
		if (!off) return p;
		return { ...p, rect: { x: p.rect.x + off.dx, y: p.rect.y + off.dy } };
	});
}

function LoupePinOverlay({
	path,
	paneId,
	sink,
	attachedTerminalId,
}: {
	path: string;
	paneId: string;
	sink: StudioSink;
	attachedTerminalId?: string;
}) {
	const qc = useQueryClient();
	const overlayRef = useRef<HTMLDivElement | null>(null);
	const [resolved, setResolved] = useState<ResolvedPin[]>([]);
	const [activePin, setActivePin] = useState<Comment | null>(null);

	const pinsQuery = useQuery({
		queryKey: ['artifact-studio', 'loupe', 'pins', path],
		queryFn: () => commentList({ artifactPath: path, includeResolved: false }),
		staleTime: 1_000,
	});
	const pins = useMemo(() => pinsQuery.data ?? [], [pinsQuery.data]);

	// Project each pin's selector through the sandboxed iframe via postMessage.
	// The child watches for scroll/resize/mutation changes and sends `pin-update`
	// messages; the host translates child-viewport rects into overlay-local coords.
	useEffect(() => {
		const overlay = overlayRef.current;
		if (!overlay) return;
		if (pins.length === 0) {
			setResolved([]);
			return;
		}

		let cancelled = false;
		let iframe: HTMLIFrameElement | null = null;
		const selectors = pins.map((p) => p.selector);

		const postToChild = (msg: M.HostToChildMessage) => {
			const cw = iframe?.contentWindow;
			if (!cw) return;
			cw.postMessage(wrapHostMessage(msg), '*');
		};

		const computeResolved = (results: M.PinResolution[]): ResolvedPin[] => {
			if (!overlay) return [];
			const overlayRect = overlay.getBoundingClientRect();
			const iframeRect = iframe?.getBoundingClientRect();
			const next: ResolvedPin[] = pins.map((pin, i) => {
				const found = results.find((r) => r.selector === pin.selector);
				if (!found?.found || !found.rect || !iframeRect) {
					return { pin, numbering: i + 1, rect: null };
				}
				const r = found.rect;
				const x = iframeRect.left + r.left + r.width / 2 - overlayRect.left;
				const y = iframeRect.top + r.top + r.height / 2 - overlayRect.top;
				return { pin, numbering: i + 1, rect: { x, y } };
			});
			return spreadOverlaps(next);
		};

		const onMessage = (e: MessageEvent) => {
			if (!M.isIkengaHostMessage(e.data)) return;
			if (e.source !== iframe?.contentWindow) return;
			const m = (e.data as M.ChildMessageWrapper).data;
			if (m.kind === 'pin-update') {
				setResolved(computeResolved(m.results));
			}
		};

		const tryAttach = () => {
			if (cancelled || iframe) return;
			const el = document.querySelector<HTMLIFrameElement>(`[data-pane-id="${paneId}"] iframe`);
			if (!el) return;
			iframe = el;
			window.addEventListener('message', onMessage);
			postToChild({ kind: 'watch-pins', selectors });
		};

		let pollHandle: ReturnType<typeof setTimeout> | null = null;
		const poll = () => {
			if (cancelled) return;
			if (iframe) return;
			tryAttach();
			pollHandle = setTimeout(poll, 200);
		};
		poll();

		return () => {
			cancelled = true;
			if (pollHandle) clearTimeout(pollHandle);
			window.removeEventListener('message', onMessage);
			postToChild({ kind: 'unwatch-pins' });
		};
	}, [pins, paneId]);

	const onRoutePin = useCallback(
		async (pin: Comment) => {
			const ts = useTerminalStore.getState();
			let preferredPtyId: string | null = null;
			let overrideSink: ReturnType<typeof studioSinkToRouteOverride>;

			if (attachedTerminalId) {
				// Embedded-terminal mode: pins route to the attached PTY,
				// bypassing the sink-popover choice entirely.
				const attached = ts.tabs.find((t) => t.id === attachedTerminalId);
				preferredPtyId = attached?.ptyId ?? null;
				overrideSink = 'terminal';
			} else {
				// No-attachment path (Phase A): prefer sink-encoded PTY,
				// fall back to focused tab.
				const activeTabPtyId = ts.tabs.find((t) => t.id === ts.activeId)?.ptyId ?? null;
				preferredPtyId = studioSinkToPreferredPtyId(sink) ?? activeTabPtyId;
				overrideSink = studioSinkToRouteOverride(sink);
			}
			try {
				const res = await routePin({ id: pin.id, preferredPtyId, overrideSink });
				// Routing is otherwise invisible — the clipboard sink in
				// particular has no in-app effect — so always echo where it went.
				console.info('[loupe] pin routed:', routeOutcomeLabel(res));
				qc.invalidateQueries({ queryKey: ['artifact-studio', 'loupe', 'pins', path] });
			} catch (e) {
				console.error('[loupe] pin route failed', e);
			}
		},
		[qc, path, sink, attachedTerminalId]
	);

	const onResolvePin = useCallback(
		async (pinId: number) => {
			try {
				await commentSetStatus(pinId, 'resolved');
				setActivePin(null);
				qc.invalidateQueries({ queryKey: ['artifact-studio', 'loupe', 'pins', path] });
			} catch (e) {
				console.error('[loupe] pin resolve failed', e);
			}
		},
		[qc, path]
	);

	const live = resolved.filter((r) => r.rect !== null);
	const stale = resolved.filter((r) => r.rect === null);

	// Compute the screen-space anchor for the active pin so the popover can
	// be portalled outside the panel (which has overflow:hidden via
	// react-resizable-panels and would otherwise clip the right edge).
	const activeScreenAnchor = useMemo(() => {
		if (!activePin) return null;
		const local = resolved.find((r) => r.pin.id === activePin.id)?.rect ?? null;
		if (!local) return null;
		const overlayRect = overlayRef.current?.getBoundingClientRect();
		if (!overlayRect) return null;
		return { x: overlayRect.left + local.x, y: overlayRect.top + local.y };
	}, [activePin, resolved]);

	return (
		<>
			<div ref={overlayRef} className="pointer-events-none absolute inset-0">
				{live.map((r) => (
					<LoupePinDot
						key={r.pin.id}
						pin={r.pin}
						numbering={r.numbering}
						x={r.rect!.x}
						y={r.rect!.y}
						onClick={() => setActivePin(r.pin)}
					/>
				))}
				{stale.length > 0 && <StalePinStrip pins={stale} onSelect={(pin) => setActivePin(pin)} />}
			</div>
			{activePin &&
				createPortal(
					<PinReviewPopover
						pin={activePin}
						screenAnchor={activeScreenAnchor}
						onClose={() => setActivePin(null)}
						onRoute={() => void onRoutePin(activePin)}
						onResolve={() => void onResolvePin(activePin.id)}
					/>,
					document.body
				)}
		</>
	);
}

function LoupePinDot({
	pin,
	numbering,
	x,
	y,
	onClick,
}: {
	pin: Comment;
	numbering: number;
	x: number;
	y: number;
	onClick: () => void;
}) {
	const tone =
		pin.status === 'open'
			? 'bg-destructive text-white'
			: pin.status === 'in_progress'
				? 'bg-[var(--achievement)] text-[var(--achievement-soft)]'
				: 'bg-[var(--live)] text-white';
	return (
		<button
			type="button"
			onClick={(e) => {
				e.stopPropagation();
				onClick();
			}}
			className={cn(
				'pointer-events-auto absolute flex h-5 w-5 -translate-x-1/2 -translate-y-1/2 items-center justify-center rounded-full border-2 border-background font-mono text-[10px] font-bold shadow transition-transform hover:scale-110 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset',
				tone
			)}
			style={{ left: `${x}px`, top: `${y}px` }}
			aria-label={`Pin ${numbering}: ${pin.text} (${pin.selector})`}
			title={`${pin.selector} — ${pin.text}`}
		>
			{numbering}
		</button>
	);
}

function StalePinStrip({
	pins,
	onSelect,
}: {
	pins: ResolvedPin[];
	onSelect: (pin: Comment) => void;
}) {
	return (
		<div className="pointer-events-auto absolute right-2 top-2 flex max-h-[60%] flex-col gap-1 overflow-y-auto rounded border border-border bg-background/95 p-1 shadow-md backdrop-blur-sm">
			<div className="px-1 font-mono text-[9px] font-bold uppercase tracking-[0.14em] text-muted-foreground">
				Stale ({pins.length})
			</div>
			{pins.map((r) => (
				<button
					key={r.pin.id}
					type="button"
					onClick={() => onSelect(r.pin)}
					className="flex items-center gap-1.5 rounded px-1.5 py-1 text-left font-mono text-[10px] text-muted-foreground hover:bg-accent hover:text-accent-foreground"
					title={`${r.pin.selector} — ${r.pin.text}`}
				>
					<span className="flex h-4 w-4 shrink-0 items-center justify-center rounded-full bg-muted text-[9px] font-bold">
						{r.numbering}
					</span>
					<span className="max-w-[140px] truncate">{r.pin.text}</span>
				</button>
			))}
		</div>
	);
}

const POPOVER_WIDTH_PX = 288; // matches w-72
const POPOVER_MARGIN_PX = 8;
const POPOVER_EST_HEIGHT_PX = 220;

const PIN_REVIEW_HEADER_ID = 'pin-review-popover-header';

function PinReviewPopover({
	pin,
	screenAnchor,
	onClose,
	onRoute,
	onResolve,
}: {
	pin: Comment;
	/** Dot anchor in viewport (screen) coordinates. `null` when the pin is
	 *  stale (no matching DOM node) — popover centres on screen instead. */
	screenAnchor: { x: number; y: number } | null;
	onClose: () => void;
	onRoute: () => void;
	onResolve: () => void;
}) {
	const containerRef = useRef<HTMLDivElement>(null);
	useFocusTrap(containerRef, { enabled: true, initialFocusSelector: 'button' });

	const [screenshotUrl, setScreenshotUrl] = useState<string | null>(null);

	// Close on Escape / outside click.
	useEffect(() => {
		const onKey = (e: KeyboardEvent) => {
			if (e.key === 'Escape') onClose();
		};
		const onDoc = (e: MouseEvent) => {
			const t = e.target as HTMLElement | null;
			if (t?.closest('[data-pin-review-popover]')) return;
			onClose();
		};
		window.addEventListener('keydown', onKey);
		window.addEventListener('mousedown', onDoc, true);
		return () => {
			window.removeEventListener('keydown', onKey);
			window.removeEventListener('mousedown', onDoc, true);
		};
	}, [onClose]);

	// Lazy-load the saved element screenshot from disk. Pin screenshots are
	// small (single cropped element), so fsRead → data URL is fine.
	useEffect(() => {
		if (!pin.screenshotPath) return;
		let cancelled = false;
		(async () => {
			try {
				const res = await fsRead(pin.screenshotPath!);
				if (cancelled) return;
				const bytes = new Uint8Array(res.bytes);
				let bin = '';
				for (let i = 0; i < bytes.length; i++) bin += String.fromCharCode(bytes[i]);
				const b64 = btoa(bin);
				setScreenshotUrl(`data:image/png;base64,${b64}`);
			} catch {
				if (!cancelled) setScreenshotUrl(null);
			}
		})();
		return () => {
			cancelled = true;
		};
	}, [pin.screenshotPath]);

	// Anchor the popover next to the dot when one exists; otherwise centre
	// it on screen (stale pins have no on-canvas anchor). Position is in
	// viewport coords because we portal to <body> to escape the panel's
	// overflow:hidden clipping.
	const style: React.CSSProperties = useMemo(() => {
		if (!screenAnchor) {
			return {
				position: 'fixed',
				left: '50%',
				top: '50%',
				transform: 'translate(-50%, -50%)',
			};
		}
		const vw = typeof window !== 'undefined' ? window.innerWidth : 1024;
		const vh = typeof window !== 'undefined' ? window.innerHeight : 768;
		const rightSide = screenAnchor.x + 14;
		const leftSide = screenAnchor.x - 14 - POPOVER_WIDTH_PX;
		// Prefer right of the dot; flip to the left if it would clip the
		// viewport, but only if the flipped position fits.
		const wantsLeft = rightSide + POPOVER_WIDTH_PX + POPOVER_MARGIN_PX > vw;
		const left = wantsLeft && leftSide >= POPOVER_MARGIN_PX ? leftSide : rightSide;
		const clampedLeft = Math.max(
			POPOVER_MARGIN_PX,
			Math.min(left, vw - POPOVER_WIDTH_PX - POPOVER_MARGIN_PX)
		);
		const rawTop = screenAnchor.y + 14;
		const clampedTop = Math.max(
			POPOVER_MARGIN_PX,
			Math.min(rawTop, vh - POPOVER_EST_HEIGHT_PX - POPOVER_MARGIN_PX)
		);
		return { position: 'fixed', left: `${clampedLeft}px`, top: `${clampedTop}px` };
	}, [screenAnchor]);

	return (
		<div
			ref={containerRef}
			data-pin-review-popover
			role="dialog"
			aria-modal="true"
			aria-labelledby={PIN_REVIEW_HEADER_ID}
			className="pointer-events-auto z-50 w-72 rounded border border-border bg-background shadow-xl"
			style={style}
		>
			<div className="flex items-center justify-between border-b border-border px-2.5 py-1.5">
				<span
					id={PIN_REVIEW_HEADER_ID}
					className="font-mono text-[10px] font-bold uppercase tracking-[0.14em] text-muted-foreground"
				>
					Pin #{pin.id} · {pin.status}
				</span>
				<button
					type="button"
					onClick={onClose}
					aria-label="Close pin review"
					className="text-muted-foreground hover:text-foreground"
				>
					<X className="h-3 w-3" />
				</button>
			</div>
			{screenshotUrl && (
				<div className="flex justify-center border-b border-border bg-muted/30 p-2">
					<img
						src={screenshotUrl}
						alt="Pinned element"
						className="max-h-32 max-w-full object-contain"
					/>
				</div>
			)}
			<div className="space-y-2 px-2.5 py-2 text-xs">
				<div className="font-mono text-[10px] text-muted-foreground" title={pin.selector}>
					<span className="truncate">{pin.selector}</span>
				</div>
				<div className="italic text-foreground">"{pin.text}"</div>
			</div>
			<div className="flex gap-1 border-t border-border p-1.5">
				<button
					type="button"
					onClick={onRoute}
					className="flex-1 rounded border border-border bg-background px-2 py-1 font-mono text-[10px] font-bold uppercase tracking-[0.10em] hover:border-foreground/60 hover:bg-foreground/5"
				>
					Route
				</button>
				{pin.status !== 'resolved' && (
					<button
						type="button"
						onClick={onResolve}
						className="flex-1 rounded border border-[var(--live)] bg-[var(--live)]/10 px-2 py-1 font-mono text-[10px] font-bold uppercase tracking-[0.10em] text-[var(--live)] hover:bg-[var(--live)]/20"
					>
						Resolve
					</button>
				)}
			</div>
		</div>
	);
}

// ─── DOM inspector ────────────────────────────────────────────────────
//
// Per the unified plan's Open Question 7 (locked in Phase 2): the iyke
// viewer-server injects an iframe bridge into every served artifact;
// the bridge responds to `iyke://dom-request` by serializing the
// document's accessibility tree. The `iyke_dom_query` Tauri command
// (commands/iyke.rs) wraps that RPC for in-shell consumers — this
// component is the first one. Auto-refreshes on fs_watch of the
// artifact's parent dir so post-save renders pick up the new tree.

function DomInspector({ paneId, path }: { paneId: string; path: string }) {
	const [result, setResult] = useState<IykeDomResult | null>(null);
	const [error, setError] = useState<string | null>(null);
	const [loading, setLoading] = useState(false);
	const [filter, setFilter] = useState('');

	const refresh = useCallback(
		async (q?: string) => {
			setLoading(true);
			setError(null);
			try {
				const out = await iykeDomQuery({
					pane: paneId,
					query: q && q.length > 0 ? q : undefined,
				});
				setResult(out);
			} catch (e) {
				setError(e instanceof Error ? e.message : String(e));
			} finally {
				setLoading(false);
			}
		},
		[paneId]
	);

	useEffect(() => {
		void refresh();
	}, [refresh]);

	// Auto-refresh on file save. Watch the artifact's parent dir (same
	// pattern HtmlFrame uses for iframe hot-reload) and debounce so a
	// burst of Create+Modify events only fires one DOM probe.
	useEffect(() => {
		let cancelled = false;
		let watcherId: string | null = null;
		let unlisten: (() => void) | null = null;
		let debounceTimer: ReturnType<typeof setTimeout> | null = null;

		void (async () => {
			try {
				const slash = path.lastIndexOf('/');
				const parent = slash > 0 ? path.slice(0, slash) : path;
				const id = await fsWatch(parent);
				if (cancelled) {
					void fsUnwatch(id);
					return;
				}
				watcherId = id;
				unlisten = await fsListenWatch(id, () => {
					if (cancelled) return;
					if (debounceTimer) clearTimeout(debounceTimer);
					// 250ms past the iframe-reload debounce (100ms) so the new
					// tree reflects the rendered output, not the pre-reload one.
					debounceTimer = setTimeout(() => {
						void refresh(filter);
					}, 250);
				});
			} catch {
				// Watcher best-effort; the manual refresh button still works.
			}
		})();

		return () => {
			cancelled = true;
			if (debounceTimer) clearTimeout(debounceTimer);
			if (unlisten) unlisten();
			if (watcherId) void fsUnwatch(watcherId);
		};
	}, [path, refresh, filter]);

	const onFilterChange = useCallback(
		(next: string) => {
			setFilter(next);
			void refresh(next);
		},
		[refresh]
	);

	return (
		<div className="flex h-full w-full flex-col">
			<div className="flex shrink-0 items-center gap-1.5 border-b border-border bg-muted/10 px-2 py-1.5">
				<TreePine className="h-3 w-3 text-muted-foreground" />
				<input
					type="text"
					value={filter}
					onChange={(e) => onFilterChange(e.target.value)}
					placeholder="filter (role / name / value)…"
					className="flex-1 rounded border border-border bg-background px-1.5 py-0.5 font-mono text-[10px] placeholder:text-muted-foreground/60 focus:outline-none focus:ring-1 focus:ring-ring/50"
				/>
				<button
					type="button"
					onClick={() => refresh(filter)}
					disabled={loading}
					title="Refresh DOM tree"
					aria-label="Refresh DOM tree"
					className="flex h-5 w-5 items-center justify-center rounded text-muted-foreground transition-colors hover:bg-accent hover:text-accent-foreground disabled:opacity-40"
				>
					<RefreshCw className={cn('h-3 w-3', loading && 'animate-spin')} />
				</button>
				{result && (
					<span className="font-mono text-[9px] uppercase tracking-[0.10em] text-muted-foreground">
						gen {result.generation}
					</span>
				)}
			</div>
			<div className="flex-1 min-h-0 overflow-auto p-2 font-mono text-[11px] leading-snug">
				{error ? (
					<div className="text-destructive">DOM probe failed: {error}</div>
				) : !result ? (
					<div className="text-muted-foreground">{loading ? 'Probing…' : 'No data yet.'}</div>
				) : result.text.length === 0 ? (
					<div className="text-muted-foreground">
						Empty tree.{' '}
						{filter ? 'No matches for the current filter.' : 'Iframe may still be loading.'}
					</div>
				) : (
					<pre className="whitespace-pre text-foreground/90">{result.text}</pre>
				)}
			</div>
		</div>
	);
}

// ─── Chrome ──────────────────────────────────────────────────────────

interface StudioChromeProps {
	path: string;
	paneId: string;
	artifactPath: string;
	dirty: boolean;
	manifest: ArtifactManifest | null;
	commentMode: boolean;
	textEditMode: boolean;
	sink: StudioSink;
	attachedTerminalId?: string;
	onAttachTerminal: (tabId: string) => void;
	onShowTerminalTab: () => void;
	onDetachTerminal: () => void;
	onCommentModeToggle: () => void;
	onTextEditModeToggle: () => void;
	onSinkOpen: () => void;
	onSave: () => void;
	onPromote: () => void;
	onPinToggle: () => void;
	onClose: () => void;
}

function StudioChrome({
	path,
	paneId,
	artifactPath,
	dirty,
	manifest,
	commentMode,
	textEditMode,
	sink,
	attachedTerminalId,
	onAttachTerminal,
	onShowTerminalTab,
	onDetachTerminal,
	onCommentModeToggle,
	onTextEditModeToggle,
	onSinkOpen,
	onSave,
	onPromote,
	onPinToggle,
	onClose,
}: StudioChromeProps) {
	const name = manifest?.name ?? path.split('/').filter(Boolean).pop() ?? 'Artifact';
	const pinSuggested = manifest?.pin?.suggested === true;

	return (
		<div className="flex shrink-0 items-center gap-1.5 border-b border-border bg-muted/20 px-2 py-1 text-xs">
			<span className="font-medium text-foreground">{name}</span>
			{dirty && (
				<span
					className="h-1.5 w-1.5 rounded-full bg-[var(--achievement)]"
					title="Unsaved changes — press ⌘S"
					role="status"
					aria-label="Unsaved changes"
				/>
			)}
			<span className="ml-auto flex items-center gap-0.5">
				<IconButton
					onClick={onCommentModeToggle}
					active={commentMode}
					title="Comment mode — click an element to annotate"
					aria-label="Toggle comment mode"
				>
					<SquareDashedMousePointer className="h-3.5 w-3.5" />
				</IconButton>
				<IconButton
					onClick={onTextEditModeToggle}
					active={textEditMode}
					title="Text-edit mode — click an element to edit its text"
					aria-label="Toggle text-edit mode"
				>
					<Pencil className="h-3.5 w-3.5" />
				</IconButton>
				<IconButton
					onClick={onPinToggle}
					active={pinSuggested}
					disabled={!manifest}
					title={pinSuggested ? 'Pin suggested (on)' : 'Pin suggested (off)'}
					aria-label="Toggle pin suggested"
				>
					<PinGlyph
						className={cn('h-3.5 w-3.5', pinSuggested && 'fill-current text-[var(--achievement)]')}
					/>
				</IconButton>
				<IconButton onClick={onPromote} title="Promote to folder…" aria-label="Promote to folder">
					<FolderTree className="h-3.5 w-3.5" />
				</IconButton>
				{attachedTerminalId ? (
					<TerminalChip
						tabId={attachedTerminalId}
						onClick={onShowTerminalTab}
						onDetach={onDetachTerminal}
					/>
				) : (
					<StudioTerminalAttachButton
						paneId={paneId}
						artifactPath={artifactPath}
						onAttach={(tabId) => {
							onAttachTerminal(tabId);
							// Pop the agent tab into focus so the new terminal is
							// immediately visible.
							onShowTerminalTab();
						}}
					/>
				)}
				<IconButton
					onClick={onSinkOpen}
					active={sink !== 'inherit'}
					title={`Pin routing: ${sink}`}
					aria-label="Pin routing destination"
					data-studio-sink-anchor
				>
					<SinkIcon className="h-3.5 w-3.5" />
				</IconButton>
				<IconButton
					onClick={onSave}
					disabled={!dirty}
					title={dirty ? 'Save (⌘S)' : 'Saved'}
					aria-label="Save artifact"
				>
					<Save className="h-3.5 w-3.5" />
				</IconButton>
				<IconButton
					onClick={onClose}
					title="Close Studio (back to preview)"
					aria-label="Close Studio"
				>
					<X className="h-3.5 w-3.5" />
				</IconButton>
			</span>
		</div>
	);
}
