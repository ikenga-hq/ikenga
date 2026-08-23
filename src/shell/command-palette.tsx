import { Command } from 'cmdk';
import {
	CheckSquare,
	FileText,
	FolderKanban,
	FolderOpen,
	Globe,
	Inbox,
	Layers,
	Mail,
	MessageSquare,
	Newspaper,
	Pin as PinIconGlyph,
	Plus,
	RefreshCw,
	Settings,
	Sparkles,
	Terminal as TerminalIcon,
	Users,
	Wallet,
} from 'lucide-react';
import { useEffect, useLayoutEffect, useRef, useState } from 'react';
import { CommandRow, type CommandRowProps } from '@/components/ui/command-row';
import { useFocusReturn, useFocusTrap } from '@/lib/a11y/focus';
import { findLeaf } from '@/lib/panes/pane-reducer';
import { usePaneStore } from '@/lib/panes/pane-store';
import type { PaneNode, PaneView } from '@/lib/panes/types';
import { fuzzyMatchSection, slugifySectionId, usePinsStore } from '@/lib/shell/pins-store';
import { useShellStore } from '@/lib/shell/shell-store';
import { buildClaudeWrappedCmd } from '@/terminal/claude-wrap';
import { createTerminalSession } from '@/terminal/single-terminal';
import { useTerminalTitles, type TerminalTitleResolver } from '@/terminal/use-terminal-titles';
import { ChromePickerDialog } from './chrome-picker/chrome-picker-dialog';
import { ActionsGroup } from './palette-actions';

export type PaletteMode = 'all' | 'views' | 'switcher' | 'projects';

interface CommandPaletteProps {
	open: boolean;
	mode: PaletteMode;
	onOpenChange: (open: boolean) => void;
}

export function CommandPalette({ open, mode, onOpenChange }: CommandPaletteProps) {
	const navigateFocused = usePaneStore((s) => s.navigateFocused);

	// When set, the palette body swaps to the inline "pin to activity bar" form
	// (replaces the old window.prompt/confirm/alert chain, which hijacked AT
	// focus). Holds the route path being pinned.
	const [pinTarget, setPinTarget] = useState<string | null>(null);

	// The Attach-Chrome picker is a sibling dialog opened from a palette row. It
	// must outlive the palette panel (which unmounts on `open=false`), so its
	// state lives here and the dialog renders above the early `!open` guard.
	const [chromePickerOpen, setChromePickerOpen] = useState(false);

	function startAttachChrome() {
		onOpenChange(false);
		// Defer so the palette unmounts before the picker grabs focus.
		setTimeout(() => setChromePickerOpen(true), 0);
	}

	function go(to: string) {
		onOpenChange(false);
		// setTimeout so the dialog can unmount before nav fires (avoids focus
		// ping-pong between the input and the destination route).
		setTimeout(() => navigateFocused(to), 0);
	}

	/**
	 * Entry point for "Pin to activity bar". Captures the route currently
	 * showing in the focused pane and opens the inline pin form (`PinForm`
	 * below) inside the palette panel. Replaces the old window.prompt /
	 * confirm / alert chain, which hijacked AT focus and broke keyboard
	 * navigation (WCAG 2.4.3 / 2.1.1). The form keeps focus inside the dialog
	 * and returns it to the trigger on close.
	 */
	function startPinFlow() {
		const focusedId = usePaneStore.getState().focusedId;
		const leaf = findLeaf(usePaneStore.getState().root, focusedId);
		const tab = leaf?.tabs[leaf.activeTabIdx];
		if (!tab || tab.kind !== 'route') {
			onOpenChange(false);
			return;
		}
		setPinTarget(tab.path);
	}

	function addToFocused(make: () => PaneView) {
		onOpenChange(false);
		setTimeout(() => {
			const view = make();
			const focusedId = usePaneStore.getState().focusedId;
			usePaneStore.getState().addTab(focusedId, view);
		}, 0);
	}

	const panelRef = useRef<HTMLDivElement | null>(null);
	// Entrance animation: mount at -8px / opacity-0, then flip to the resting
	// state one tick later so the transition runs. The global
	// prefers-reduced-motion safety net in styles.css collapses the duration.
	const [entered, setEntered] = useState(false);
	useLayoutEffect(() => {
		if (!open) {
			setEntered(false);
			// Closing resets any in-progress pin form so the next open starts
			// on the command list, not a stale form.
			setPinTarget(null);
			return;
		}
		const id = requestAnimationFrame(() => setEntered(true));
		return () => cancelAnimationFrame(id);
	}, [open]);

	// The Attach-Chrome picker renders independently of the palette panel so it
	// survives the palette closing (the row that opens it closes the palette).
	const chromePicker = (
		<ChromePickerDialog open={chromePickerOpen} onOpenChange={setChromePickerOpen} />
	);

	if (!open) return chromePicker;

	const placeholder =
		mode === 'views'
			? 'Open in focused pane…'
			: mode === 'switcher'
				? 'Switch to open tab…'
				: mode === 'projects'
					? 'Switch project…'
					: 'Type a command or search…';

	return (
		<>
			{chromePicker}
			<div
				className="fixed inset-0 z-50 flex items-start justify-center pt-[15vh]"
				onClick={() => onOpenChange(false)}
			>
				<div
					className="absolute inset-0 bg-black/50 transition-opacity duration-[var(--motion-fast)] ease-[var(--ease-calm)]"
					aria-hidden
				/>
				<div
					ref={panelRef}
					role="dialog"
					aria-modal="true"
					aria-label={pinTarget !== null ? 'Pin to activity bar' : 'Command palette'}
					tabIndex={-1}
					data-open={entered ? 'true' : 'false'}
					className="relative w-full max-w-xl overflow-hidden rounded-lg border border-border bg-popover text-popover-foreground shadow-2xl outline-none transition-[opacity,transform] duration-[var(--motion-fast)] ease-[var(--ease-calm)] data-[open=false]:-translate-y-2 data-[open=false]:opacity-0 data-[open=true]:translate-y-0 data-[open=true]:opacity-100"
					onClick={(e) => e.stopPropagation()}
				>
					{pinTarget !== null ? (
						<PinForm
							target={pinTarget}
							onClose={() => {
								setPinTarget(null);
								onOpenChange(false);
							}}
							onCancel={() => setPinTarget(null)}
						/>
					) : (
						<Command label="Command palette" className="flex flex-col">
							<Command.Input
								autoFocus
								placeholder={placeholder}
								className="w-full border-b border-border bg-transparent px-4 py-3 text-sm outline-none placeholder:text-muted-foreground"
							/>
							<Command.List className="max-h-[50vh] overflow-y-auto p-2">
								<Command.Empty className="py-8 text-center text-sm text-muted-foreground">
									No results.
								</Command.Empty>

								{mode === 'switcher' ? (
									<SwitcherGroup onClose={() => onOpenChange(false)} />
								) : mode === 'projects' ? (
									<ProjectsGroup onClose={() => onOpenChange(false)} />
								) : (
									<>
										<Command.Group
											heading="In focused pane"
											className="text-xs text-muted-foreground"
										>
											<PaletteItem
												onSelect={() =>
													addToFocused(() => ({
														kind: 'terminal',
														sessionId: createTerminalSession(),
													}))
												}
												Icon={Plus}
												label="New Terminal"
												shortcut="⌃T"
											/>
											<PaletteItem
												onSelect={() =>
													addToFocused(() => ({
														kind: 'terminal',
														sessionId: createTerminalSession({
															cmd: buildClaudeWrappedCmd(),
															title: 'claude',
														}),
													}))
												}
												Icon={Plus}
												label="New Claude Terminal"
												shortcut="⌃⇧T"
											/>
											<PaletteItem
												onSelect={() => go('/projects/new-artifact')}
												Icon={Sparkles}
												label="New artifact…"
												shortcut="⌘⇧N"
											/>
											<PaletteItem
												onSelect={() => onOpenChange(false)}
												disabled={true}
												Icon={RefreshCw}
												label="Switch Adapter (coming soon)"
												shortcut="⌘⇧A"
											/>
											<PaletteItem
												onSelect={() => onOpenChange(false)}
												Icon={FolderOpen}
												label="Open File (use Files mode for now)"
											/>
											<PaletteItem
												onSelect={startPinFlow}
												Icon={PinIconGlyph}
												label="Pin focused route to activity bar…"
											/>
											<PaletteItem
												onSelect={startAttachChrome}
												Icon={Globe}
												label="Attach Chrome profile / tab…"
											/>
										</Command.Group>

										{mode === 'all' && <ActionsGroup onClose={() => onOpenChange(false)} />}

										{mode === 'all' && (
											<Command.Group heading="Navigate" className="text-xs text-muted-foreground">
												<PaletteItem
													onSelect={() => go('/mail/inbox')}
													Icon={Inbox}
													label="Go to Inbox"
												/>
												<PaletteItem
													onSelect={() => go('/mail/triage')}
													Icon={Inbox}
													label="Go to Triage"
												/>
												<PaletteItem
													onSelect={() => go('/mail/drafts')}
													Icon={Mail}
													label="Go to Reply Drafts"
												/>
												<PaletteItem
													onSelect={() => go('/tasks')}
													Icon={CheckSquare}
													label="Go to Tasks"
												/>
												<PaletteItem
													onSelect={() => go('/pkg/com.ikenga.work/delegations')}
													Icon={Users}
													label="Go to Delegations"
												/>
												<PaletteItem
													onSelect={() => go('/finance')}
													Icon={Wallet}
													label="Go to Finance"
												/>
												<PaletteItem
													onSelect={() => go('/outbox/email')}
													Icon={Mail}
													label="Go to Outbox · Email"
												/>
												<PaletteItem
													onSelect={() => go('/outbox/newsletter')}
													Icon={Newspaper}
													label="Go to Outbox · Newsletter"
												/>
												<PaletteItem
													onSelect={() => go('/outbox/social')}
													Icon={MessageSquare}
													label="Go to Outbox · Social"
												/>
												<PaletteItem
													onSelect={() => go('/outbox/sent')}
													Icon={Mail}
													label="Go to Outbox · Sent"
												/>
												<PaletteItem
													onSelect={() => go('/outbox/approvals')}
													Icon={CheckSquare}
													label="Go to Outbox · Approvals"
												/>
												<PaletteItem
													onSelect={() => go('/settings')}
													Icon={Settings}
													label="Go to Settings"
												/>
											</Command.Group>
										)}
									</>
								)}
							</Command.List>
						</Command>
					)}
				</div>
			</div>
		</>
	);
}

interface SwitcherEntry {
	paneId: string;
	tabIdx: number;
	label: string;
	view: PaneView;
}

function collectOpenTabs(node: PaneNode, resolveTerminal: TerminalTitleResolver): SwitcherEntry[] {
	if (node.type === 'leaf') {
		return node.tabs.map((view, tabIdx) => ({
			paneId: node.id,
			tabIdx,
			label: viewLabelShort(view, resolveTerminal),
			view,
		}));
	}
	return node.children.flatMap((child) => collectOpenTabs(child, resolveTerminal));
}

function viewLabelShort(view: PaneView, resolveTerminal: TerminalTitleResolver): string {
	switch (view.kind) {
		case 'route':
			return `Route ${view.path}`;
		case 'terminal': {
			// A uuid slice is unsearchable — you can't type "claude" to find the
			// terminal running claude. The real label is, and it doubles as the
			// palette's fuzzy-match text.
			const t = resolveTerminal(view.sessionId);
			return t ? `Terminal · ${t.label}` : `Terminal · ${view.sessionId.slice(0, 8)}`;
		}
		case 'artifact':
			return `Artifact ${view.path}`;
		case 'artifact-studio':
			return 'Artifact studio';
		case 'scratchpad':
			return `Scratchpad ${view.name}`;
	}
}

function SwitcherGroup({ onClose }: { onClose: () => void }) {
	const root = usePaneStore((s) => s.root);
	const focusedId = usePaneStore((s) => s.focusedId);
	const focusPane = usePaneStore((s) => s.focusPane);
	const switchTab = usePaneStore((s) => s.switchTab);
	const resolveTerminal = useTerminalTitles();

	const entries = collectOpenTabs(root, resolveTerminal);

	function focusEntry(entry: SwitcherEntry) {
		onClose();
		setTimeout(() => {
			focusPane(entry.paneId);
			switchTab(entry.paneId, entry.tabIdx);
		}, 0);
	}

	return (
		<Command.Group heading="Open tabs" className="text-xs text-muted-foreground">
			{entries.map((entry) => {
				const leaf = findLeaf(root, entry.paneId);
				const isActive =
					leaf !== null && entry.paneId === focusedId && leaf.activeTabIdx === entry.tabIdx;
				return (
					<PaletteItem
						key={`${entry.paneId}-${entry.tabIdx}`}
						onSelect={() => focusEntry(entry)}
						Icon={iconForView(entry.view)}
						label={entry.label}
						detail={isActive ? '(focused)' : `pane ${entry.paneId.slice(0, 6)}`}
					/>
				);
			})}
			{entries.length === 0 && (
				<div role="status" className="px-3 py-3 text-xs text-muted-foreground">
					No open tabs.
				</div>
			)}
		</Command.Group>
	);
}

function ProjectsGroup({ onClose }: { onClose: () => void }) {
	const projects = useShellStore((s) => s.projects);
	const activeProjectId = useShellStore((s) => s.activeProjectId);
	const setActiveProject = useShellStore((s) => s.setActiveProject);
	const navigateFocused = usePaneStore((s) => s.navigateFocused);

	// Active first; archived last. Match the activity-bar indicator order so
	// the palette and the popover read the same.
	const sorted = projects.slice().sort((a, b) => {
		if (a.id === activeProjectId) return -1;
		if (b.id === activeProjectId) return 1;
		const aArc = a.archived_at != null ? 1 : 0;
		const bArc = b.archived_at != null ? 1 : 0;
		if (aArc !== bArc) return aArc - bArc;
		if (a.position !== b.position) return a.position - b.position;
		return a.created_at - b.created_at;
	});

	function pick(id: string) {
		onClose();
		// Defer so the dialog can unmount before the optimistic state flip
		// triggers a re-render of the activity bar.
		setTimeout(() => {
			void setActiveProject(id);
		}, 0);
	}

	function openManage() {
		onClose();
		setTimeout(() => navigateFocused('/settings/projects'), 0);
	}

	return (
		<>
			<Command.Group heading="Projects" className="text-xs text-muted-foreground">
				{sorted.map((p) => (
					<PaletteItem
						key={p.id}
						value={`${p.display_name} ${p.id} ${p.description ?? ''}`}
						onSelect={() => pick(p.id)}
						leading={
							<>
								<span
									aria-hidden
									className="inline-block h-3 w-3 shrink-0 rounded-full border border-border"
									style={{ background: p.color ?? 'var(--fg-faint)' }}
								/>
								{p.icon ? (
									<span className="leading-none">{p.icon}</span>
								) : (
									<FolderKanban className="h-4 w-4 shrink-0 text-muted-foreground" />
								)}
							</>
						}
						label={p.display_name}
						trailing={
							<>
								{p.id === activeProjectId && (
									<span className="shrink-0 text-[10px] uppercase text-muted-foreground">
										Active
									</span>
								)}
								{p.archived_at != null && (
									<span className="shrink-0 text-[10px] uppercase text-muted-foreground">
										Archived
									</span>
								)}
							</>
						}
					/>
				))}
				{sorted.length === 0 && (
					<div role="status" className="px-3 py-3 text-xs text-muted-foreground">
						No projects loaded.
					</div>
				)}
			</Command.Group>
			<Command.Group heading="Manage" className="text-xs text-muted-foreground">
				<PaletteItem onSelect={openManage} Icon={FolderKanban} label="Open Projects settings…" />
			</Command.Group>
		</>
	);
}

function iconForView(view: PaneView): typeof Inbox {
	switch (view.kind) {
		case 'route':
			return Layers;
		case 'terminal':
			return TerminalIcon;
		case 'artifact':
			return FolderOpen;
		case 'artifact-studio':
			return FolderOpen;
		case 'scratchpad':
			return FileText;
	}
}

/**
 * Inline "pin to activity bar" form rendered inside the palette panel. Replaces
 * the old window.prompt/confirm/alert chain (which hijacked AT focus and broke
 * keyboard nav — WCAG 2.4.3 / 2.1.1). Focus is trapped within the form and
 * returned to the trigger by the parent's `useFocusReturn`. Two steps: a
 * label + optional section input, then — when the section is new — a confirm
 * step to create it. Errors render inline (role="alert") instead of `alert()`.
 */
function PinForm({
	target,
	onClose,
	onCancel,
}: {
	target: string;
	/** Pin committed (or no-op done) — close the whole palette. */
	onClose: () => void;
	/** Step back out of the pin form to the command list. */
	onCancel: () => void;
}) {
	const formRef = useRef<HTMLDivElement | null>(null);
	const [label, setLabel] = useState(() => target.replace(/^\//, '') || 'Home');
	const [sectionInput, setSectionInput] = useState('');
	// 'edit' = label/section entry; 'confirm-section' = confirm creating a new
	// section before committing the pin.
	const [step, setStep] = useState<'edit' | 'confirm-section'>('edit');
	const [pendingSlug, setPendingSlug] = useState<string | null>(null);
	const [error, setError] = useState<string | null>(null);
	const [busy, setBusy] = useState(false);

	// Trap Tab/Shift+Tab inside the form; initial focus on the label input.
	useFocusTrap(formRef, { enabled: true, initialFocusSelector: '#pin-label-input' });

	async function commit(sectionId: string | null) {
		setBusy(true);
		setError(null);
		try {
			await usePinsStore.getState().addPin({
				kind: 'route',
				target,
				label: label.trim(),
				sectionId,
			});
			onClose();
		} catch (e) {
			setError(`Could not pin: ${String(e)}`);
			setBusy(false);
		}
	}

	async function onSubmit(e: React.FormEvent) {
		e.preventDefault();
		if (busy) return;
		const trimmedLabel = label.trim();
		if (!trimmedLabel) {
			setError('A label is required.');
			return;
		}
		const section = sectionInput.trim();
		if (!section) {
			await commit(null);
			return;
		}
		const store = usePinsStore.getState();
		await store.hydrate();
		const matched = fuzzyMatchSection(section, store.sections);
		if (matched) {
			await commit(matched.id);
			return;
		}
		const candidate = slugifySectionId(section);
		if (!candidate) {
			setError(`'${section}' is not a valid section name.`);
			return;
		}
		setPendingSlug(candidate);
		setStep('confirm-section');
	}

	async function confirmCreateSection() {
		if (busy || !pendingSlug) return;
		const section = sectionInput.trim();
		setBusy(true);
		setError(null);
		try {
			const created = await usePinsStore.getState().createSection({
				id: pendingSlug,
				label: section,
				iconLucide: 'folder',
			});
			await commit(created.id);
		} catch (err) {
			setError(`Could not create section: ${String(err)}`);
			setBusy(false);
		}
	}

	const btnBase =
		'rounded-md px-3 py-1.5 text-sm outline-none transition-colors motion-reduce:transition-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset disabled:cursor-not-allowed disabled:opacity-50';

	return (
		<form onSubmit={onSubmit} className="flex flex-col">
			<div ref={formRef} className="flex flex-col">
				<div className="border-b border-border px-4 py-3 font-medium text-sm">
					Pin to activity bar
				</div>
				<div className="flex flex-col gap-3 p-4">
					<div className="text-xs text-muted-foreground">
						Pinning <span className="font-mono text-foreground">{target}</span>
					</div>
					<label
						className="flex flex-col gap-1 text-xs text-muted-foreground"
						htmlFor="pin-label-input"
					>
						Label
						<input
							id="pin-label-input"
							value={label}
							onChange={(e) => setLabel(e.target.value)}
							disabled={step !== 'edit' || busy}
							className="rounded-md border border-border bg-transparent px-3 py-2 text-sm text-foreground outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset"
						/>
					</label>
					<label
						className="flex flex-col gap-1 text-xs text-muted-foreground"
						htmlFor="pin-section-input"
					>
						Section (optional)
						<input
							id="pin-section-input"
							value={sectionInput}
							onChange={(e) => setSectionInput(e.target.value)}
							disabled={step !== 'edit' || busy}
							placeholder="Existing sections match fuzzily"
							className="rounded-md border border-border bg-transparent px-3 py-2 text-sm text-foreground outline-none placeholder:text-muted-foreground focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset"
						/>
					</label>

					{step === 'confirm-section' && (
						<div role="status" className="text-xs text-muted-foreground">
							Create section{' '}
							<span className="font-medium text-foreground">{sectionInput.trim()}</span> (id:{' '}
							<span className="font-mono text-foreground">{pendingSlug}</span>)?
						</div>
					)}

					{error && (
						<div role="alert" className="text-xs text-destructive">
							{error}
						</div>
					)}
				</div>

				<div className="flex justify-end gap-2 border-t border-border px-4 py-3">
					{step === 'edit' ? (
						<>
							<button
								type="button"
								onClick={onCancel}
								disabled={busy}
								className={`${btnBase} border border-border hover:bg-accent hover:text-accent-foreground`}
							>
								Cancel
							</button>
							<button
								type="submit"
								disabled={busy}
								className={`${btnBase} bg-primary text-primary-foreground hover:bg-primary/90`}
							>
								Pin
							</button>
						</>
					) : (
						<>
							<button
								type="button"
								onClick={() => {
									setStep('edit');
									setPendingSlug(null);
								}}
								disabled={busy}
								className={`${btnBase} border border-border hover:bg-accent hover:text-accent-foreground`}
							>
								Back
							</button>
							<button
								type="button"
								onClick={() => {
									void confirmCreateSection();
								}}
								disabled={busy}
								className={`${btnBase} bg-primary text-primary-foreground hover:bg-primary/90`}
							>
								Create &amp; pin
							</button>
						</>
					)}
				</div>
			</div>
		</form>
	);
}

// ⌘K palette rows share the consolidated `CommandRow` (size `md`); this thin
// alias just binds the size so the many call sites above stay terse.
function PaletteItem(props: Omit<CommandRowProps, 'size' | 'as'>) {
	return <CommandRow size="md" {...props} />;
}

interface PaletteState {
	open: boolean;
	mode: PaletteMode;
}

/**
 * Wires ⌘K / Ctrl+K to open the palette in 'all' mode. Other entry
 * points (⌘T views, ⌘P switcher) live in `Workspace` and call
 * `setOpen(true, mode)` on the returned controller.
 */
export function useCommandPalette() {
	const [state, setState] = useState<PaletteState>({ open: false, mode: 'all' });
	const navigateFocused = usePaneStore((s) => s.navigateFocused);

	// Return focus to the trigger element when the palette closes (Esc, scrim
	// click, row select). cmdk manages its own internal focus while open but
	// leaves focus orphaned on the unmounted panel at close (WCAG 2.4.3).
	useFocusReturn(state.open);

	useEffect(() => {
		function onKey(e: KeyboardEvent) {
			const mod = e.metaKey || e.ctrlKey;
			const key = e.key.toLowerCase();
			if (mod && key === 'k') {
				e.preventDefault();
				setState((s) => ({ open: !s.open, mode: 'all' }));
			} else if (e.key === 'Escape') {
				setState({ open: false, mode: 'all' });
			}
		}
		window.addEventListener('keydown', onKey);
		return () => window.removeEventListener('keydown', onKey);
	}, [navigateFocused]);

	return {
		open: state.open,
		mode: state.mode,
		setOpen: (open: boolean, mode: PaletteMode = 'all') => setState({ open, mode }),
	};
}
