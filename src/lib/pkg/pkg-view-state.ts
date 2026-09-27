// WP-45 — D-08 `pkg-view` pane states: the pure half.
//
// `designs/pane-chrome.html?state=pkg-view` and its five variants
// (`pkg-loading`, `pkg-consent`, `pkg-crashed`, `pkg-sidecar-down`,
// `pkg-blocked`). The React half lives in
// `src/components/pkg/pkg-view-states.tsx`; everything here is plain data so
// it can be unit-tested without mounting the iframe host.

/** Every state root the pkg hosts render carries one of these as
 *  `data-state` (G-55 state map). `pkg-view` is the resting, healthy view. */
export type PkgViewStateName =
	| 'pkg-view'
	| 'pkg-loading'
	| 'pkg-consent'
	| 'pkg-crashed'
	| 'pkg-sidecar-down'
	| 'pkg-blocked';

// ─── pkg-loading: handshake steps ────────────────────────────────────────────

/** Where the iframe host is in its boot. Mirrors the host's own effects:
 *  `fetch` = Step 1 (`pkgContentHtml` in flight), `handshake` = Step 2
 *  (AppBridge connected, waiting for the view's `ui/initialize`), `ready` =
 *  the bridge fired `initialized`. Route resolution happens before the host
 *  mounts, so it is always done by the time a phase exists. */
export type PkgBootPhase = 'fetch' | 'handshake' | 'ready';

export type HandshakeStepStatus = 'done' | 'now' | 'todo';

export interface HandshakeStep {
	id: 'resolve' | 'fetch' | 'handshake' | 'paint';
	label: string;
	status: HandshakeStepStatus;
}

const PHASE_ORDER: Record<PkgBootPhase, number> = { fetch: 1, handshake: 2, ready: 4 };

/** The step list under the ember pulse — "the difference between 'it is
 *  slow' and 'it is stuck on the bridge'" (pane-chrome.html `pkg-loading`).
 *  Labels are the design's, with the real pkg id / route substituted. */
export function handshakeSteps(
	phase: PkgBootPhase,
	pkgId: string,
	source: string
): HandshakeStep[] {
	const at = PHASE_ORDER[phase];
	const status = (idx: number): HandshakeStepStatus =>
		idx < at ? 'done' : idx === at ? 'now' : 'todo';
	return [
		{ id: 'resolve', label: `kernel · ui_routes resolved  ${pkgId}`, status: status(0) },
		{ id: 'fetch', label: `pkg_content · html fetched  ${source}`, status: status(1) },
		{
			id: 'handshake',
			label: 'AppBridge · handshake  (hostContext, supabase keys)',
			status: status(2),
		},
		{ id: 'paint', label: 'view · first paint', status: status(3) },
	];
}

/** How long the loading overlay waits for the view's `ui/initialize` before
 *  stepping aside. Not a crash: a view that never initialises keeps rendering
 *  exactly as it did before WP-45 — the overlay just stops covering it. */
export const HANDSHAKE_OVERLAY_TIMEOUT_MS = 5000;

/** Grace after the iframe's `load` before the overlay steps aside anyway. A
 *  bridge view answers `ui/initialize` well inside it; a view that renders
 *  without the AppBridge is no longer held behind the overlay for the full
 *  HANDSHAKE_OVERLAY_TIMEOUT_MS (WP-45 review F5). */
export const HANDSHAKE_AFTER_LOAD_GRACE_MS = 600;

// ─── pkg-consent: parked trust review ────────────────────────────────────────

/** The lines the inline consent prompt lists: every leaf of the pending
 *  review's `new_capabilities` snapshot that the last-approved snapshot does
 *  not carry, as `path.to.leaf = value`. Both sides are the kernel's
 *  normalized JSON strings (`PkgTrustReview`); anything unparseable yields
 *  `[]` and the prompt falls back to its generic sentence. */
export function addedCapabilityLines(oldJson: string, newJson: string): string[] {
	let before: unknown;
	let after: unknown;
	try {
		before = oldJson ? JSON.parse(oldJson) : {};
		after = JSON.parse(newJson);
	} catch {
		return [];
	}
	const had = new Set(flatten(before));
	return flatten(after).filter((line) => !had.has(line));
}

function flatten(v: unknown, prefix = ''): string[] {
	if (Array.isArray(v)) return v.flatMap((x) => flatten(x, prefix));
	if (v && typeof v === 'object') {
		return Object.entries(v as Record<string, unknown>).flatMap(([k, x]) =>
			flatten(x, prefix ? `${prefix}.${k}` : k)
		);
	}
	return prefix ? [`${prefix} = ${String(v)}`] : [String(v)];
}

// ─── pkg-blocked: CSP violations ─────────────────────────────────────────────

/** The subset of `SecurityPolicyViolationEvent` the host reads. */
export interface CspViolationLike {
	blockedURI: string;
	effectiveDirective: string;
}

export interface PkgBlockedInfo {
	/** Host shown in the heading (`fal.media`), or the raw CSP keyword
	 *  (`inline`, `eval`) when the blocked thing was not a URL. */
	host: string;
	/** Full blocked URL / keyword, for the detail line. */
	target: string;
	/** `csp:<directive>` for iframe CSP blocks, `webview:allowed_origins`
	 *  for kernel-side navigation blocks. Fed to the trust sheet's
	 *  violation `scope`. */
	scope: string;
	/** Webview blocks only: the serialized origin the kernel rejected — what
	 *  "Allow host…" grants (`pkg_webview_allow_origin`). */
	origin?: string;
}

/** Can "Allow host…" actually lift this block? Only a kernel-side webview
 *  origin rejection: the user can grant the origin additively. An iframe CSP
 *  block comes from the package's own policy (the shell injects none), which
 *  no host-side grant can override. */
export function canAllowHost(info: PkgBlockedInfo): boolean {
	return info.scope === WEBVIEW_ORIGIN_SCOPE && Boolean(info.origin);
}

const WEBVIEW_ORIGIN_SCOPE = 'webview:allowed_origins';

const SCRIPT_DIRECTIVES = new Set(['script-src', 'script-src-elem', 'script-src-attr']);

/** Heading for the `pkg-blocked` state. "Blocked a navigation to …" only
 *  when a navigation was what got stopped (design copy); script and other
 *  resource blocks say what they were, and a keyword target (`inline`,
 *  `eval`) is not dressed up as a host (WP-45 review F4). */
export function blockedHeading(info: PkgBlockedInfo): string {
	if (info.scope === WEBVIEW_ORIGIN_SCOPE) return `Blocked a navigation to ${info.host}`;
	const directive = info.scope.startsWith('csp:') ? info.scope.slice(4) : info.scope;
	const isKeyword = info.target === 'inline' || info.target === 'eval';
	if (NAVIGATION_DIRECTIVES.has(directive)) return `Blocked a navigation to ${info.host}`;
	if (SCRIPT_DIRECTIVES.has(directive) || directive === 'default-src') {
		if (info.target === 'inline') return 'Blocked an inline script';
		if (info.target === 'eval') return 'Blocked a script eval';
		return `Blocked a script from ${info.host}`;
	}
	return isKeyword ? `Blocked ${info.target} content` : `Blocked a load from ${info.host}`;
}

/** Directives whose violation means a navigation / frame load was stopped —
 *  the design's "Blocked a navigation to …" case. Resource-level blocks
 *  (an image, a font) after the view is up do not replace the view. */
const NAVIGATION_DIRECTIVES = new Set(['frame-src', 'child-src', 'form-action', 'navigate-to']);

/** Directives whose violation before the view initialises means its boot
 *  script never ran — there is no working view to keep. */
const BOOT_DIRECTIVES = new Set(['script-src', 'script-src-elem', 'default-src']);

/** Should this violation replace the view with `pkg-blocked`? Yes for any
 *  navigation directive; yes for a script block inside the boot window (its
 *  boot was stopped); otherwise no — a blocked image, font or fetch never
 *  took a view down before WP-45 and still doesn't. The host logs it and the
 *  view keeps running.
 *
 *  `bootWindowClosed` is true once the view initialised OR the loading
 *  overlay stepped aside (load grace / timeout). A view that never sends
 *  `ui/initialize` must not stay "booting" forever, or any later script
 *  violation would take a working view down (WP-45 review F4). */
export function isBlockingViolation(v: CspViolationLike, bootWindowClosed: boolean): boolean {
	if (NAVIGATION_DIRECTIVES.has(v.effectiveDirective)) return true;
	return !bootWindowClosed && BOOT_DIRECTIVES.has(v.effectiveDirective);
}

export function blockedInfoFromViolation(v: CspViolationLike): PkgBlockedInfo {
	const target = v.blockedURI || 'inline';
	return { host: hostOf(target), target, scope: `csp:${v.effectiveDirective || 'unknown'}` };
}

/** Payload of the Rust `pkg://navigation-blocked` event (`pkg/webview.rs`). */
export interface NavigationBlockedEvent {
	pkgId: string;
	paneId: string;
	url: string;
	origin: string;
	allowedOrigins: string[];
}

export const NAVIGATION_BLOCKED_EVENT = 'pkg://navigation-blocked';

export function blockedInfoFromNavigation(ev: NavigationBlockedEvent): PkgBlockedInfo {
	return {
		host: hostOf(ev.url || ev.origin),
		target: ev.url || ev.origin,
		scope: WEBVIEW_ORIGIN_SCOPE,
		origin: ev.origin,
	};
}

function hostOf(target: string): string {
	try {
		const u = new URL(target);
		return u.host || target;
	} catch {
		return target;
	}
}

// ─── pane `⋯` menu, pkg branch ───────────────────────────────────────────────

/** `/pkg/<pkgId>/<splat>` → `pkgId`, else null. Same shape `useWebviewRoute`
 *  and the pkg route catch-all match on. */
export function pkgIdFromRoutePath(path: string): string | null {
	const m = path.match(/^\/pkg\/([^/?#]+)/);
	return m ? decodeURIComponent(m[1]) : null;
}

/** Where "Report violation log" lands: the Ngwa Health violations panel
 *  (`/settings/pkg-audit` redirects to the same place). */
export const VIOLATION_LOG_PATH = '/ngwa/health?section=violations';

/** Where "View permissions" / "Package settings" land: the Ngwa item detail
 *  (it owns the Settings + Permissions tabs; there is no tab deep-link yet). */
export function itemDetailPath(pkgId: string): string {
	return `/ngwa/item/${encodeURIComponent(pkgId)}`;
}
