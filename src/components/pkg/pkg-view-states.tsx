// WP-45 — D-08 `pkg-view` pane states, built on WP-43's shared state
// components (`src/components/states/`, consumed, never edited here).
//
// Design: `designs/pane-chrome.html?state=<name>` for each of
//   pkg-loading · pkg-consent · pkg-crashed · pkg-sidecar-down · pkg-blocked
// Spec: `drafts/design-spec-D-03-07.md` §D-08 `pkg-view`.
//
// Every state root carries `data-state="<name>"` (G-55) and offers exactly
// one next action (WP-43's `StateAction` is singular by construction). The
// design's secondary buttons (Open devtools, Keep blocking, Report violation
// log, …) live in the pane `⋯` menu instead — see `pkg-pane-menu.tsx`.
//
// Pure logic (step list, violation classification, capability diff) is in
// `@/lib/pkg/pkg-view-state` so it is testable without a DOM host.

import { Check, Circle, CircleDot, Lock, Plug, ShieldAlert } from 'lucide-react';
import { useState } from 'react';
import { createPortal } from 'react-dom';
import { EmptyState, ErrorState, LoadingState, OfflineState } from '@/components/states';
import { useNgwaSnapshot } from '@/lib/ngwa/use-ngwa-snapshot';
import {
	addedCapabilityLines,
	blockedHeading,
	canAllowHost,
	handshakeSteps,
	type PkgBlockedInfo,
	type PkgBootPhase,
} from '@/lib/pkg/pkg-view-state';
import { type PkgTrustReview, pkgWebviewAllowOrigin } from '@/lib/tauri-cmd';
import { NgwaTrustSheet } from '@/shell/ngwa/ngwa-trust-sheet';

const MONO_DETAIL: React.CSSProperties = {
	fontFamily: 'var(--font-mono)',
	fontSize: 'var(--text-micro)',
	color: 'var(--fg-muted)',
	background: 'var(--bg-sunken)',
	border: '1px solid var(--border)',
	borderRadius: 'var(--radius-sm)',
	padding: 'var(--space-2)',
	textAlign: 'left',
	wordBreak: 'break-all',
	whiteSpace: 'pre-wrap',
};

// ─── pkg-loading ─────────────────────────────────────────────────────────────

export interface PkgLoadingStateProps {
	pkgId: string;
	source: string;
	phase: PkgBootPhase;
	/** Overlay mode: cover the (already-mounted, still-handshaking) iframe
	 *  instead of replacing it, so the frame keeps loading underneath. */
	overlay?: boolean;
}

/** Ember pulse + handshake step text — never a spinner (LoadingState owns
 *  the pulse). The design's "Cancel" is not offered: LoadingState has no
 *  action by contract ("nothing to act on mid-flight"). */
export function PkgLoadingState({ pkgId, source, phase, overlay }: PkgLoadingStateProps) {
	const steps = handshakeSteps(phase, pkgId, source);
	const state = (
		<LoadingState
			data-state="pkg-loading"
			fill
			heading="Loading package…"
			body={
				<ol className="mt-1 flex flex-col gap-1 text-left" aria-label="Handshake steps">
					{steps.map((s) => {
						const Icon = s.status === 'done' ? Check : s.status === 'now' ? CircleDot : Circle;
						return (
							<li
								key={s.id}
								data-step={s.id}
								data-step-status={s.status}
								aria-current={s.status === 'now' ? 'step' : undefined}
								className="flex items-center gap-2"
								style={{
									fontFamily: 'var(--font-mono)',
									color:
										s.status === 'done'
											? 'var(--fg-muted)'
											: s.status === 'now'
												? 'var(--fg)'
												: 'var(--fg-faint)',
								}}
							>
								<Icon
									aria-hidden="true"
									className="size-3 shrink-0"
									style={{ color: s.status === 'now' ? 'var(--ember)' : undefined }}
								/>
								<span>{s.label}</span>
							</li>
						);
					})}
				</ol>
			}
		/>
	);
	if (!overlay) return state;
	// Overlay mode: an opaque ground so the half-painted iframe underneath
	// doesn't show through while the bridge handshakes.
	return (
		<div className="absolute inset-0 z-10" style={{ background: 'var(--bg-base)' }}>
			{state}
		</div>
	);
}

// ─── pkg-consent ─────────────────────────────────────────────────────────────

export interface PkgConsentStateProps {
	pkgId: string;
	review: PkgTrustReview;
	onAllow: () => void;
	busy?: boolean;
	error?: string | null;
}

/** The view is parked by the kernel because its manifest now asks for
 *  capabilities the user has not approved (`pkg_trust_list_pending`).
 *  Inline in the pane — not a modal. One action: allow (→
 *  `pkg_trust_approve`, which re-registers the pkg). Declining is just not
 *  allowing; rejecting uninstalls, which stays in Ngwa.
 *
 *  Accepted deviation from the design (WP-45 review F2 — carry into the PR
 *  call-outs + 05 WP-45 DoD note): the design draws a consent *bar* over a
 *  still-running view with Allow once / Always for this project / Deny. Here
 *  the kernel has parked the pkg (no routes registered, nothing running), so
 *  the state is full-pane with the single Allow WP-43 permits; reject stays
 *  in Ngwa. Callers must ignore Allow while `busy` (review F3). */
export function PkgConsentState({ pkgId, review, onAllow, busy, error }: PkgConsentStateProps) {
	const lines = addedCapabilityLines(review.old_capabilities, review.new_capabilities);
	return (
		<EmptyState
			data-state="pkg-consent"
			fill
			icon={ShieldAlert}
			heading={`${pkgId} wants permissions you have not granted`}
			body={
				<div className="flex flex-col gap-2">
					<span>
						Version {review.manifest_version} asks for more than the set you approved, so the view is
						paused until you allow it.
					</span>
					{lines.length > 0 && <div style={MONO_DETAIL}>{lines.join('\n')}</div>}
					{error && <span style={{ color: 'var(--danger)' }}>{error}</span>}
				</div>
			}
			action={{ label: busy ? 'Allowing…' : 'Allow', onClick: onAllow }}
		/>
	);
}

// ─── pkg-crashed ─────────────────────────────────────────────────────────────

export interface PkgCrashedStateProps {
	pkgId: string;
	source: string;
	error: string;
	onReload: () => void;
}

/** Copy is the shipped host's ("Failed to load package UI") plus the
 *  design's reassurance line; the raw error stays visible for debugging. */
export function PkgCrashedState({ pkgId, source, error, onReload }: PkgCrashedStateProps) {
	return (
		<ErrorState
			data-state="pkg-crashed"
			fill
			heading="Failed to load package UI"
			body={
				<div className="flex flex-col gap-2">
					<div style={MONO_DETAIL}>
						{error}
						{'\n'}
						{pkgId} · {source} · iframe host
					</div>
					<span>The pane kept the frame so you do not lose your place. Nothing was written.</span>
				</div>
			}
			action={{ label: 'Reload view', onClick: onReload }}
		/>
	);
}

// ─── pkg-sidecar-down ────────────────────────────────────────────────────────

export interface PkgSidecarDownStripProps {
	reason: string;
	onRestart: () => void;
	busy?: boolean;
}

/** "View fine, sidecar stopped: strip with Restart." OfflineState (warn
 *  tone, not error) laid out as a one-line strip above the still-live view.
 *  The icon is sized down to the strip (`[&>svg]:size-4` beats
 *  OfflineState's own `size-7`). Deviations from the design strip: its "Log"
 *  link is the ⋯ menu's Report violation log / Ngwa Health, and there is no
 *  dismiss × — the strip clears itself when the sidecar comes back. */
export function PkgSidecarDownStrip({ reason, onRestart, busy }: PkgSidecarDownStripProps) {
	return (
		<OfflineState
			data-state="pkg-sidecar-down"
			icon={Plug}
			heading="Sidecar stopped"
			body={`${reason} — the view still works; anything that needs the sidecar does not.`}
			action={{ label: busy ? 'Restarting…' : 'Restart', onClick: onRestart }}
			className="min-h-0 shrink-0 flex-row flex-wrap justify-start gap-x-3 gap-y-1 border-b px-3 py-1.5 text-left [&>div]:max-w-none [&>svg]:size-4"
		/>
	);
}

// ─── pkg-blocked ─────────────────────────────────────────────────────────────

export interface PkgBlockedStateProps {
	pkgId: string;
	blocked: PkgBlockedInfo;
	/** Webview blocks: opens the trust review sheet (below) with only this
	 *  origin to grant. */
	onAllowHost?: () => void;
	/** Iframe CSP blocks: "Allow host…" cannot lift a policy the package
	 *  wrote itself, so the state's one action is Reload view instead. */
	onReload?: () => void;
}

export function PkgBlockedState({ pkgId, blocked, onAllowHost, onReload }: PkgBlockedStateProps) {
	const allowable = canAllowHost(blocked);
	const action =
		allowable && onAllowHost
			? { label: 'Allow host…', onClick: onAllowHost }
			: onReload
				? { label: 'Reload view', onClick: onReload }
				: undefined;
	return (
		<ErrorState
			data-state="pkg-blocked"
			fill
			icon={Lock}
			heading={blockedHeading(blocked)}
			body={
				<div className="flex flex-col gap-2">
					<span>
						{allowable ? (
							<>
								This host is not in the package&rsquo;s{' '}
								<code>capabilities.webview.allowed_origins</code>. The kernel stopped the load
								before anything was fetched.
							</>
						) : (
							<>
								The package&rsquo;s own content security policy (
								<code>{blocked.scope.replace(/^csp:/, '')}</code>) stopped this load. Allow host is
								not available here: the shell adds no policy of its own, so only the package
								author can allow this source.
							</>
						)}
					</span>
					<div style={MONO_DETAIL}>
						blocked {blocked.target} · pkg {pkgId}
					</div>
				</div>
			}
			action={action}
		/>
	);
}

/** "Allow host…" → the existing Ngwa trust review sheet in `violation` mode,
 *  scoped to the one blocked origin: its grant is `pkg_webview_allow_origin`
 *  (additive, persisted), not the pkg-wide trust grant, and it hides Revoke
 *  trust. Mounted only while open so the Ngwa snapshot query (a cold scan
 *  can take a while) never runs for a pane that is simply showing its view.
 *  Portaled to `document.body` so a pooled iframe surface's stacking context
 *  (z-30, overflow hidden) can't cap or clip it (review F7). Renders nothing
 *  for a block that cannot be allowed. */
export function PkgBlockedTrustSheet({
	pkgId,
	blocked,
	open,
	onOpenChange,
	onApproved,
}: {
	pkgId: string;
	blocked: PkgBlockedInfo;
	open: boolean;
	onOpenChange: (open: boolean) => void;
	/** Called once the origin is granted — re-attempt the blocked load. */
	onApproved?: () => void;
}) {
	if (!open || !canAllowHost(blocked)) return null;
	return createPortal(
		<BlockedTrustSheetInner
			pkgId={pkgId}
			blocked={blocked}
			onOpenChange={onOpenChange}
			onApproved={onApproved}
		/>,
		document.body
	);
}

function BlockedTrustSheetInner({
	pkgId,
	blocked,
	onOpenChange,
	onApproved,
}: {
	pkgId: string;
	blocked: PkgBlockedInfo;
	onOpenChange: (open: boolean) => void;
	onApproved?: () => void;
}) {
	const { items } = useNgwaSnapshot();
	const item = items.find((i) => i.id === pkgId) ?? null;
	const origin = blocked.origin ?? blocked.target;
	return (
		<NgwaTrustSheet
			open
			onOpenChange={onOpenChange}
			item={item}
			mode="violation"
			violationScopeKind={blocked.scope}
			violationTarget={origin}
			violationGrant={{
				label: `Allow ${blocked.host}`,
				pendingLabel: 'Allowing…',
				run: async () => {
					await pkgWebviewAllowOrigin(pkgId, origin);
				},
			}}
			onApproved={onApproved}
		/>
	);
}

/** Local open/close state for the sheet, shared by both hosts. */
export function useAllowHostSheet() {
	const [open, setOpen] = useState(false);
	return { open, setOpen, openSheet: () => setOpen(true) };
}
