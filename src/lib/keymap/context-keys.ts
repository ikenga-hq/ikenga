// The context-key service (G-ACTIONS §4.3) — the **only** producer of the
// values a `when` evaluates against. Two sources:
//
// 1. **Focus-within markers.** A surface declares "focus inside me means
//    <area>Focus" by carrying `data-ctx-focus="<area>"` (space-separated for
//    more than one; `focusMarkerProps()` builds it). Until each owning WP adds
//    its marker, the shipped DOM is read through the fallback selectors
//    below (`data-terminal-session`, `data-companion-dispatch`,
//    `data-explorer-section`, `data-pane-id`) — a marker and its fallback
//    mean the same thing, so adding the marker later changes nothing.
// 2. **Store derivation.** `paneKind` / `resource` come from the pane store
//    (the focused leaf's active tab), `project` from the shell store,
//    `paletteOpen` from `setPaletteOpen()` (the palette's listener, WP-54).
//
// `computeContextKeys` is pure (inputs → snapshot) and is what the tests
// drive; `getContextKeys` gathers the live inputs and calls it.

import { findLeaf, getActiveView } from '@/lib/panes/pane-reducer';
import { usePaneStore } from '@/lib/panes/pane-store';
import type { PaneView } from '@/lib/panes/types';
import { useFilesStore } from '@/lib/shell/files-store';
import { useShellStore } from '@/lib/shell/shell-store';
import { type EvalOptions, isTypingTarget, type WhenContext } from './when';

// ─── Vocabulary (frozen, DEC-62 + the §4.3 freeze additions) ──────────────

export const CONTEXT_KEYS = {
	inputFocus: 'boolean',
	terminalFocus: 'boolean',
	explorerFocus: 'boolean',
	filesFocus: 'boolean',
	paneFocus: 'boolean',
	paneKind: 'string',
	resource: 'string',
	resourceExtname: 'string',
	project: 'string',
	// Added at the freeze under G-71 (§7.3 package-key `when`s).
	sessionFocus: 'boolean',
	ngwaItemFocus: 'boolean',
	ngwaItemKind: 'string',
	// Added at the freeze for owner widgets (B-21 additive rule).
	dispatchFocus: 'boolean',
	paletteOpen: 'boolean',
	// Added by WP-56 under the same B-21 additive rule, for its migrated
	// leftover handlers (G-ACTIONS §10.2 "Reserved for WP-56").
	permissionCardFocus: 'boolean',
	approveGateFocus: 'boolean',
	loupeFocus: 'boolean',
	pinComposerFocus: 'boolean',
	markdownEditorFocus: 'boolean',
	// Fix round 1 (B-21): narrower than `approveGateFocus` (the whole
	// section, incl. the draft queue) — true only inside the detail pane,
	// matching the pre-WP-56 scoping of ⌘S / ⌘↵ (`onDetailKeyDown` was on
	// `.ob-detail` alone; J/K stayed section-wide).
	approveGateDetailFocus: 'boolean',
} as const;

export type ContextKeyName = keyof typeof CONTEXT_KEYS;

export function isKnownContextKey(key: string): key is ContextKeyName {
	return Object.prototype.hasOwnProperty.call(CONTEXT_KEYS, key);
}

/** Keys bound to DOM focus / palette state — undefined in a menu context
 *  (§1.3), so a placement `when` naming one is `W_FOCUS_IN_PLACEMENT`. */
export const FOCUS_CONTEXT_KEYS: readonly ContextKeyName[] = [
	'inputFocus',
	'terminalFocus',
	'explorerFocus',
	'filesFocus',
	'paneFocus',
	'sessionFocus',
	'ngwaItemFocus',
	'dispatchFocus',
	'paletteOpen',
	'permissionCardFocus',
	'approveGateFocus',
	'loupeFocus',
	'pinComposerFocus',
	'markdownEditorFocus',
	'approveGateDetailFocus',
];

/** The snapshot shape. Every key is present; `undefined` means "no value". */
export type ContextKeys = { [K in ContextKeyName]: (typeof CONTEXT_KEYS)[K] extends 'boolean' ? boolean : string | undefined };

// ─── Focus-within markers ─────────────────────────────────────────────────

export const CTX_FOCUS_ATTR = 'data-ctx-focus';
/** Absolute path of a focused row (Files / Artifacts / Scratchpads rows). */
export const CTX_RESOURCE_ATTR = 'data-ctx-resource';
/** `NgwaKind` of a focused Ngwa item row. */
export const CTX_NGWA_KIND_ATTR = 'data-ctx-ngwa-kind';

export type FocusArea =
	| 'explorer'
	| 'files'
	| 'pane'
	| 'session'
	| 'ngwa-item'
	| 'dispatch'
	| 'terminal'
	| 'permission-card'
	| 'approve-gate'
	| 'approve-gate-detail'
	| 'loupe'
	| 'pin-composer'
	| 'markdown-editor';

/** Spread onto a surface's root: `<div {...focusMarkerProps('dispatch')}>`. */
export function focusMarkerProps(...areas: FocusArea[]): { [CTX_FOCUS_ATTR]: string } {
	return { [CTX_FOCUS_ATTR]: areas.join(' ') };
}

function markerSelector(area: FocusArea): string {
	return `[${CTX_FOCUS_ATTR}~="${area}"]`;
}

/** Shipped-DOM equivalents of each marker (5b / v0.14.0). Row-level areas
 *  are "inside that Explorer section, not on its header row". */
const FALLBACK_SELECTORS: Partial<Record<FocusArea, string>> = {
	terminal: '[data-terminal-session], .xterm',
	dispatch: '[data-companion-dispatch]',
	explorer: '[data-explorer-section]',
	pane: '[data-pane-id]',
};
const SECTION_ROW_FALLBACK: Partial<Record<FocusArea, string>> = {
	files: 'files',
	session: 'sessions',
	'ngwa-item': 'ngwa-project',
};

function closestSafe(el: Element, selector: string): Element | null {
	try {
		return el.closest(selector);
	} catch {
		return null;
	}
}

/** Is `el` (the focused element) inside a surface marked `area`? */
export function isFocusWithin(el: Element | null, area: FocusArea): boolean {
	if (!el) return false;
	if (closestSafe(el, markerSelector(area))) return true;
	const fallback = FALLBACK_SELECTORS[area];
	if (fallback && closestSafe(el, fallback)) return true;
	const section = SECTION_ROW_FALLBACK[area];
	if (section) {
		return (
			closestSafe(el, `[data-explorer-section="${section}"]`) !== null &&
			closestSafe(el, '[data-explorer-row="header"]') === null
		);
	}
	return false;
}

// ─── Palette state (set by the palette's listener, WP-54) ─────────────────

let paletteOpenState = false;

/** The ⌘K palette (any mode, incl. the Shortcuts view) opened / closed —
 *  today's `openRef.current` in `command-palette.tsx`. */
export function setPaletteOpen(open: boolean): void {
	paletteOpenState = open;
}

export function isPaletteOpen(): boolean {
	return paletteOpenState;
}

// ─── Pure computation ─────────────────────────────────────────────────────

export interface ContextInputs {
	/** The focused element (`document.activeElement`, or the event target). */
	activeElement: Element | null;
	/** The focused leaf's active tab; `null` with no pane. */
	pane: PaneView | null;
	/** The Files section's selected row — the fallback for a focused Files
	 *  row that carries no `data-ctx-resource`. */
	filesSelectedPath: string | null;
	/** Active project; `rootPath` null for the path-less default project. */
	project: { id: string; rootPath: string | null } | null;
	paletteOpen: boolean;
}

/** `.ts` for `/a/b/c.TS`; `''` when the basename has no extension (or is a
 *  dotfile like `.gitignore`); undefined for an undefined resource. */
export function extnameOf(resource: string | undefined): string | undefined {
	if (resource === undefined) return undefined;
	const norm = resource.replace(/\\/g, '/');
	const base = norm.slice(norm.lastIndexOf('/') + 1);
	const dot = base.lastIndexOf('.');
	if (dot <= 0) return '';
	return base.slice(dot).toLowerCase();
}

function paneResource(view: PaneView | null): string | undefined {
	if (!view) return undefined;
	switch (view.kind) {
		case 'route':
		case 'artifact':
		case 'artifact-studio':
			return view.path;
		default:
			return undefined;
	}
}

function attrWithin(el: Element | null, attr: string): string | undefined {
	if (!el) return undefined;
	const holder = closestSafe(el, `[${attr}]`);
	const v = holder?.getAttribute(attr);
	return v ? v : undefined;
}

/** Inputs → the §4.3 snapshot. Pure; the tests drive this. */
export function computeContextKeys(inputs: ContextInputs): ContextKeys {
	const el = inputs.activeElement;
	const filesFocus = isFocusWithin(el, 'files');
	const sessionFocus = isFocusWithin(el, 'session');
	const ngwaItemFocus = isFocusWithin(el, 'ngwa-item');
	const explorerFocus = filesFocus || isFocusWithin(el, 'explorer');
	const paneFocus = !explorerFocus && isFocusWithin(el, 'pane');

	let resource: string | undefined;
	if (filesFocus) resource = attrWithin(el, CTX_RESOURCE_ATTR) ?? inputs.filesSelectedPath ?? undefined;
	else if (paneFocus) resource = paneResource(inputs.pane);

	return {
		inputFocus: isTypingTarget(el),
		terminalFocus: isFocusWithin(el, 'terminal'),
		explorerFocus,
		filesFocus,
		paneFocus,
		paneKind: inputs.pane?.kind,
		resource,
		resourceExtname: extnameOf(resource),
		project: inputs.project?.rootPath ? inputs.project.id : undefined,
		sessionFocus,
		ngwaItemFocus,
		ngwaItemKind: ngwaItemFocus ? attrWithin(el, CTX_NGWA_KIND_ATTR) : undefined,
		dispatchFocus: isFocusWithin(el, 'dispatch'),
		paletteOpen: inputs.paletteOpen,
		permissionCardFocus: isFocusWithin(el, 'permission-card'),
		approveGateFocus: isFocusWithin(el, 'approve-gate'),
		approveGateDetailFocus: isFocusWithin(el, 'approve-gate-detail'),
		loupeFocus: isFocusWithin(el, 'loupe'),
		pinComposerFocus: isFocusWithin(el, 'pin-composer'),
		markdownEditorFocus: isFocusWithin(el, 'markdown-editor'),
	};
}

/**
 * The **menu context** (§1.3): the snapshot a placement `when` is evaluated
 * against, bound to the object the menu opened on — not to DOM focus. Every
 * focus key is undefined, so a placement naming one never shows.
 */
export function buildMenuContext(target: {
	resource?: string;
	paneKind?: PaneView['kind'];
	ngwaItemKind?: string;
	project?: string;
}): WhenContext {
	const out: Record<string, string | undefined> = {
		resource: target.resource,
		resourceExtname: extnameOf(target.resource),
		paneKind: target.paneKind,
		ngwaItemKind: target.ngwaItemKind,
		project: target.project,
	};
	return out;
}

// ─── Live reads ───────────────────────────────────────────────────────────

function focusedPaneView(activeElement: Element | null): PaneView | null {
	const { root, focusedId } = usePaneStore.getState();
	// Prefer the pane DOM focus is actually in; fall back to the store's
	// focused leaf (some leaf is always focused).
	const domPaneId = attrWithin(activeElement, 'data-pane-id');
	const leaf = (domPaneId ? findLeaf(root, domPaneId) : null) ?? findLeaf(root, focusedId);
	return leaf ? getActiveView(leaf) : null;
}

/** Gather the live inputs. `target` (a keydown's target) wins over
 *  `document.activeElement` when it is an element — same as the pre-v2
 *  `evaluateWhen(clause, e)` read `e.target`. */
export function readContextInputs(target?: EventTarget | null): ContextInputs {
	const doc = typeof document === 'undefined' ? null : document;
	const activeElement =
		typeof Element !== 'undefined' && target instanceof Element ? target : (doc?.activeElement ?? null);
	const shell = useShellStore.getState();
	const active = shell.activeProject;
	return {
		activeElement,
		pane: focusedPaneView(activeElement),
		filesSelectedPath: useFilesStore.getState().selectedPath,
		project: active ? { id: active.id, rootPath: active.root_path } : null,
		paletteOpen: isPaletteOpen(),
	};
}

/** The live §4.3 snapshot. */
export function getContextKeys(target?: EventTarget | null): ContextKeys {
	return computeContextKeys(readContextInputs(target));
}

/** The eval options a live `evaluateWhen` needs (project root for `=~`). */
export function getEvalOptions(): EvalOptions {
	const root = useShellStore.getState().activeProject?.root_path;
	return root ? { projectRoot: root } : {};
}
