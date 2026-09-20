// WP-20 (frame slot skeleton): the workspace-level overlays.
//
// Renders today's four overlays verbatim. They are exported as two pieces
// (`IframePoolOverlay` and `RestOverlays`) instead of one because
// `CommandPalette` — which stays in workspace.tsx (it owns the local
// `palette` state) — originally rendered between the iframe pool and the
// other three overlays; splitting here preserves the exact original DOM
// order (Do-not-touch: no DOM position/order change) instead of grouping
// them contiguously.
import { PkgIframeLayer } from '@/components/pkg/pkg-iframe-layer';
import { IFRAME_POOL_ENABLED } from '@/lib/panes/iframe-pool';
import { TerminalHandoffPrompt } from '../artifact-wizard/terminal-handoff-prompt';
import { WizardPopRecoveryChip } from '../artifact-wizard/wizard-pop-recovery-chip';
import { RuntimeBunChip } from '../runtime-bun-chip';

export function IframePoolOverlay() {
	return (
		<>
			{/* Hoisted iframe pool: owns pooled pkg iframes and floats them
			    (position:fixed, z below every overlay) over their pane rects so
			    they survive tab switch / reorder / split / pane-tree rebuild.
			    Mounted once here, outside the PanelGroup remount scope. Renders
			    nothing when pooling is off (no surfaces are ever claimed). */}
			{IFRAME_POOL_ENABLED && <PkgIframeLayer />}
		</>
	);
}

export function RestOverlays() {
	return (
		<>
			<TerminalHandoffPrompt />

			<WizardPopRecoveryChip />
			<RuntimeBunChip />
		</>
	);
}
