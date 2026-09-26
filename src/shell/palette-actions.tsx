// WP-18a — the ⌘K palette "Actions" group.
//
// Pulls every installed skill's actions (the same `list_all_skill_actions`
// Tauri command the skill-action button bar uses) up into the shipped command
// palette as a first-class group that slots BETWEEN the pane-local group and
// Navigate. cmdk owns ranking on a typed query; the group's position is only
// the tiebreaker (pane creation stays top for muscle memory, verbs that do
// work come next, raw route nav sinks to the bottom).
//
// ENTER routes through the *existing* `dispatchAction()` (action-runner.ts) —
// NOT a fork. `confirm` seeds the New-Session dialog with the prompt template;
// `approve` runs and its drafts pause at /outbox/approvals via the
// `pa-action-paused` event. Both are dispatchable today; `streaming` / `form`
// / `silent` render visible-but-disabled with a mode badge exactly like
// `ActionButton` does, so the operator sees the capability is coming.
//
// Design: plans/atelier-parity/designs/parity-palette-actions.html.

import { useQuery } from '@tanstack/react-query';
import { Command } from 'cmdk';
import {
	CheckSquare,
	Columns2,
	FileText,
	FolderOpen,
	Keyboard,
	type LucideIcon,
	Mail,
	MessageSquare,
	Monitor,
	Moon,
	PanelLeft,
	RotateCcw,
	Rows2,
	Search,
	Send,
	Sparkles,
	Sun,
	Target,
	TrendingUp,
	Users,
} from 'lucide-react';
import { CommandRow } from '@/components/ui/command-row';
import { dispatchAction, isDispatchable } from '@/components/pkg/actions/action-runner';
import { type IkengaMode, useIkengaStore } from '@/lib/ikenga/theme-store';
import { queryKeys } from '@/lib/query-keys';
import { listAllSkillActions, type SkillAction } from '@/lib/tauri-cmd';
import { useEffectiveMenu } from '@/lib/actions/store';
import { resolveMenuItems } from '@/shell/menu/resolve';

// Domain → leading glyph. Presentation-only (the manifest has no per-action
// icon field); a design choice, not a data field. Unknown / absent domains
// fall back to the generic skill spark. See the design's open questions.
const DOMAIN_ICONS: Record<string, LucideIcon> = {
	mail: Mail,
	outbound: Send,
	finance: TrendingUp,
	tasks: CheckSquare,
	sales: Users,
	content: FileText,
	research: Search,
	strategy: Target,
};

function iconForDomain(domain: string | undefined): LucideIcon {
	return (domain && DOMAIN_ICONS[domain]) || Sparkles;
}

// ux_mode → chip colour, keyed to the mode's dispatch character. Anchored to
// the live Dusk Wood tokens (--agent / --live / --achievement / --primary /
// --systemic, all shipped in @ikenga/tokens). confirm+approve dispatch today;
// the rest are the deferred (disabled) modes.
const UX_MODE_TOKEN: Record<string, string> = {
	confirm: '--agent',
	approve: '--live',
	streaming: '--achievement',
	form: '--primary',
	silent: '--systemic',
};

function UxModeChip({ uxMode, disabled }: { uxMode: string; disabled: boolean }) {
	const token = UX_MODE_TOKEN[uxMode] ?? '--fg-muted';
	return (
		<span
			className="shrink-0 rounded-[var(--radius-xs)] border px-1.5 py-0.5 font-mono text-[9px] uppercase tracking-wide"
			style={{
				color: `var(${token})`,
				borderColor: `var(${token}-soft)`,
				backgroundColor: `var(${token}-soft)`,
			}}
		>
			{uxMode}
			{disabled && <span className="ml-1 text-[8px] opacity-70">soon</span>}
		</span>
	);
}

function DomainChip({ domain }: { domain: string }) {
	return (
		<span className="shrink-0 rounded-[var(--radius-xs)] border border-border bg-muted px-1.5 py-0.5 font-mono text-[9px] uppercase tracking-wide text-muted-foreground">
			{domain}
		</span>
	);
}

/**
 * Every installed skill's actions. No pkg-install Tauri event exists to
 * invalidate on, so this leans on a ~30s staleTime (design default). Refetches
 * on window refocus like every other query, and any future pkg-install event
 * can invalidate `queryKeys.skillActions.all`.
 */
export function useAllSkillActions() {
	return useQuery({
		queryKey: queryKeys.skillActions.all,
		queryFn: listAllSkillActions,
		staleTime: 30_000,
	});
}

/** Stable cmdk key / React key for an action — `skill/verb` is the same stable
 *  key the ActionBar uses; scope with pkgId so two pkgs sharing a skill dir
 *  never collide. */
function actionKey(a: SkillAction): string {
	return `${a.pkgId}::${a.skill}/${a.verb}`;
}

/**
 * The "Actions" group. Renders nothing when no installed skill contributes
 * actions (a fresh install with only builtin pkgs) — the header is omitted
 * entirely and the rest of the palette is untouched, mirroring `ActionBar`
 * returning null on an empty list. Only mounted in `mode: 'all'` by the caller.
 */
export function ActionsGroup({ onClose }: { onClose: () => void }) {
	const { data } = useAllSkillActions();
	const actions = data ?? [];
	if (actions.length === 0) return null;

	function dispatch(action: SkillAction) {
		// Close the palette first (like every other row via onClose →
		// onOpenChange(false)); defer the dispatch a tick so the palette unmounts
		// before the New-Session dialog grabs focus — same focus-ping-pong guard
		// the `go()` navigations use. `dispatchAction` stamps the source
		// (`skill-action` vs `approve-action`) and opens the shared dialog; for
		// `approve` the run then pauses at the gate via `pa-action-paused`.
		onClose();
		setTimeout(() => {
			void dispatchAction(action);
		}, 0);
	}

	return (
		<Command.Group heading="Actions" className="text-xs text-muted-foreground">
			{actions.map((action) => {
				const canDispatch = isDispatchable(action);
				const Icon = iconForDomain(action.domain);
				return (
					<CommandRow
						key={actionKey(action)}
						size="md"
						value={`${action.name} ${action.skill} ${action.verb} ${action.domain ?? ''}`}
						Icon={Icon}
						label={action.name}
						detail={`${action.skill} / ${action.verb}`}
						disabled={!canDispatch}
						onSelect={canDispatch ? () => dispatch(action) : () => {}}
						trailing={
							<>
								{action.domain && <DomainChip domain={action.domain} />}
								<UxModeChip uxMode={action.uxMode} disabled={!canDispatch} />
							</>
						}
					/>
				);
			})}
		</Command.Group>
	);
}

// ─── WP-09 — the palette "Manage" group (mode `all`) ─────────────────────────
//
// Frame-level commands that used to live as rail buttons. The theme toggle is
// re-homed here from the rail (WP-03 / PR #215 removes `ThemeToggleButton`);
// the cycle and labels are the rail's, unchanged: light → dark → system.

export const THEME_CYCLE: Record<IkengaMode, IkengaMode> = {
	light: 'dark',
	dark: 'system',
	system: 'light',
};

const THEME_ICON: Record<IkengaMode, LucideIcon> = {
	light: Sun,
	dark: Moon,
	system: Monitor,
};

export const THEME_LABEL: Record<IkengaMode, string> = {
	light: 'Light',
	dark: 'Dark',
	system: 'System',
};

// The `palette` menu's canonical ids (G-ACTIONS §1.3) rendered here have no
// per-action icon of their own (built-ins own their surface's glyph, per
// `registry.ts`) — this is the local map.
const PALETTE_MENU_ICON: Readonly<Record<string, LucideIcon>> = {
	'pane.split-right': Columns2,
	'pane.split-down': Rows2,
	'pane.reopen': RotateCcw,
	'explorer.toggle': PanelLeft,
	'companion.toggle': MessageSquare,
	'palette.projects': FolderOpen,
	'shortcuts.open': Keyboard,
};

/** `onShowShortcuts` switches the open palette to its Shortcuts view. It is
 *  injected so this module never imports `command-palette.tsx` (which
 *  imports this one). */
export function ManageGroup({ onShowShortcuts }: { onShowShortcuts: () => void }) {
	const mode = useIkengaStore((s) => s.mode);
	const setMode = useIkengaStore((s) => s.setMode);
	const next = THEME_CYCLE[mode];
	const Icon = THEME_ICON[next];

	// The `palette` menu (G-ACTIONS §1.3): pane creation / frame toggles, plus
	// every package and user action placed here (§12, G-70) — order, hidden
	// ids and appends already applied by the merge. Every default id here is
	// already a global command (workspace.tsx, the rail, or this file's own
	// `useCommands` below), so the generic fallback (`runMenuAction`) fires it
	// correctly; only "Keyboard shortcuts" needs a local override, since its
	// owner handler lives in the caller (`command-palette.tsx`) rather than
	// the command table.
	const paletteMenu = useEffectiveMenu('palette');
	const rows = resolveMenuItems(paletteMenu, {
		handlers: { 'shortcuts.open': onShowShortcuts },
	});

	return (
		<Command.Group heading="Manage" className="text-xs text-muted-foreground">
			<CommandRow
				size="md"
				value={`toggle theme appearance light dark system ${THEME_LABEL[next]}`}
				Icon={Icon}
				label={`Toggle theme: ${THEME_LABEL[mode]} → ${THEME_LABEL[next]}`}
				onSelect={() => {
					// The palette stays open: the theme flips live behind it and a
					// second Enter keeps cycling, like repeated clicks on the old
					// rail button.
					setMode(next);
				}}
			/>
			{rows.map((row) =>
				row.kind === 'separator' ? null : (
					<CommandRow
						key={row.id}
						size="md"
						value={row.label}
						Icon={PALETTE_MENU_ICON[row.id] ?? Sparkles}
						label={row.label}
						shortcut={row.shortcut || undefined}
						onSelect={row.run}
					/>
				)
			)}
		</Command.Group>
	);
}
