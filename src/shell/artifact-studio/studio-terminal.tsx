// Studio rail Terminal tab.
//
// Mounts a SingleTerminal for the tab attached to this Studio pane.
// Empty-state renders a picker that lists side-pane-owned PTYs the user
// can claim, plus a "+ new claude in this artifact's folder" entry.
//
// Per D1 (yield the xterm), the side-pane's TerminalView shows a
// placeholder body while a tab is owned by Studio; this component is the
// other half of that swap.

import { useEffect, useMemo, useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import { ExternalLink, Plus, RefreshCcw, Terminal as TerminalIcon, X } from 'lucide-react';
import { Button } from '@/components/ui/button';
import { cn } from '@/components/ui/utils';
import { buildAgentWrappedCmd, type AgentEngineKind } from '@/terminal/claude-wrap';
import { SingleTerminal, createClaudeTerminalSession, createTerminalSession } from '@/terminal/single-terminal';
import { useTerminalStore, type TerminalTab } from '@/terminal/session-store';
import { usePaneStore } from '@/lib/panes/pane-store';
import { activeProjectCwd } from '@/lib/shell/active-project-cwd';
import { defaultShellArgv, defaultShellLabel } from '@/lib/platform';

interface StudioTerminalProps {
	paneId: string;
	artifactPath: string;
	attachedTerminalId: string | null;
	onAttach: (tabId: string) => void;
	onDetach: () => void;
}

/** Shells offered by the spawn menu in the empty-state picker. `claude` is
 *  first (most common Studio companion), then a plain shell for general
 *  work, then the other agentic CLIs the routing dispatcher recognises. */
interface ShellPreset {
	label: string;
	title: string;
	cmd: string[];
}

const SHELL_PRESETS: ShellPreset[] = [
	{ label: 'claude', title: 'claude', cmd: ['claude'] },
	{ label: defaultShellLabel(), title: defaultShellLabel(), cmd: defaultShellArgv() },
	{ label: 'codex', title: 'codex', cmd: ['codex'] },
	{ label: 'gemini', title: 'gemini', cmd: ['gemini'] },
];

export function StudioTerminal({
	paneId,
	artifactPath,
	attachedTerminalId,
	onAttach,
	onDetach,
}: StudioTerminalProps) {
	const tab = useTerminalStore((s) =>
		attachedTerminalId ? s.tabs.find((t) => t.id === attachedTerminalId) : undefined
	);

	// Reload re-attach: when this loupe mounts with a saved attachedTerminalId
	// whose tab still exists but is sidepane-owned (PTYs survive the loupe
	// remount but the owner field rehydrates as sidepane per session-store),
	// silently restore the studio attachment. D10: only the picker shows
	// when the tab id is gone entirely.
	useEffect(() => {
		if (!attachedTerminalId) return;
		const ts = useTerminalStore.getState();
		const t = ts.tabs.find((x) => x.id === attachedTerminalId);
		if (!t) return;
		if (t.status === 'exited' || t.status === 'error') return;
		if (t.owner.kind === 'sidepane') {
			ts.attachToStudio(attachedTerminalId, paneId, artifactPath);
		}
	}, [attachedTerminalId, paneId, artifactPath]);

	if (!attachedTerminalId) {
		return <StudioTerminalPicker paneId={paneId} artifactPath={artifactPath} onAttach={onAttach} />;
	}
	if (!tab) {
		return <StaleAttachmentNotice onClear={onDetach} />;
	}
	if (tab.status === 'exited' || tab.status === 'error') {
		return <ExitedAttachmentNotice tab={tab} onDetach={onDetach} />;
	}
	return (
		<div className="relative h-full w-full">
			<SingleTerminal sessionId={attachedTerminalId} />
		</div>
	);
}

// ─── Picker ──────────────────────────────────────────────────────────

interface StudioTerminalPickerProps {
	paneId: string;
	artifactPath: string;
	onAttach: (tabId: string) => void;
}

function StudioTerminalPicker({ paneId, artifactPath, onAttach }: StudioTerminalPickerProps) {
	const tabs = useTerminalStore((s) => s.tabs);
	const [conflict, setConflict] = useState<{
		tabId: string;
		previousPaneId: string;
	} | null>(null);

	const candidates = useMemo(
		() =>
			tabs.filter(
				(t) =>
					t.status === 'running' &&
					(t.owner.kind === 'sidepane' || (t.owner.kind === 'studio' && t.owner.paneId !== paneId))
			),
		[tabs, paneId]
	);

	const tryAttach = (tabId: string) => {
		const res = useTerminalStore.getState().attachToStudio(tabId, paneId, artifactPath);
		if (res.ok) {
			onAttach(tabId);
			return;
		}
		setConflict({ tabId, previousPaneId: res.previousPaneId });
	};

	const spawnCwd = activeProjectCwd();
	const spawnNew = (preset: ShellPreset) => {
		let id: string;
		if (preset.title === 'claude') {
			id = createClaudeTerminalSession({ cwd: spawnCwd }, 'claude');
		} else if (preset.title === 'codex' || preset.title === 'gemini') {
			const cmd = buildAgentWrappedCmd({ engine: preset.title as AgentEngineKind });
			id = createTerminalSession({ cwd: spawnCwd, cmd, title: preset.title });
		} else {
			id = createTerminalSession({ cwd: spawnCwd, cmd: preset.cmd, title: preset.title });
		}
		// Attach immediately. Even if the PTY hasn't finished spawning,
		// `attachToStudio` flips the owner field; the picker hides and the
		// SingleTerminal will mount once the PTY id lands in the store.
		useTerminalStore.getState().attachToStudio(id, paneId, artifactPath);
		onAttach(id);
	};

	return (
		<div className="flex h-full w-full flex-col">
			<div className="flex shrink-0 items-center gap-1.5 border-b border-border bg-muted/20 px-2 py-1 text-[10px] text-muted-foreground">
				<TerminalIcon className="h-3 w-3" />
				<span className="font-mono">attach a terminal</span>
			</div>
			<div className="flex-1 min-h-0 overflow-y-auto">
				<div className="border-b border-border px-3 py-2">
					<div className="mb-1.5 flex items-center gap-1 text-[10px] text-muted-foreground">
						<Plus className="h-3 w-3 text-amber-600 dark:text-amber-400" />
						<span className="font-mono">new in {spawnCwd}</span>
					</div>
					<div className="flex flex-wrap gap-1">
						{SHELL_PRESETS.map((preset) => (
							<button
								key={preset.title}
								type="button"
								onClick={() => spawnNew(preset)}
								className="rounded border border-border bg-background px-2 py-1 text-[11px] text-foreground transition-colors hover:bg-muted/30"
								title={preset.cmd.join(' ')}
							>
								{preset.label}
							</button>
						))}
					</div>
				</div>
				{candidates.length === 0 ? (
					<div className="px-3 py-6 text-center text-[11px] text-muted-foreground">
						No running terminals to attach.
					</div>
				) : (
					<ul>
						{candidates.map((t) => (
							<li key={t.id}>
								<button
									type="button"
									onClick={() => tryAttach(t.id)}
									className="flex w-full items-start gap-2 border-b border-border px-3 py-2 text-left transition-colors hover:bg-muted/30"
								>
									<TerminalIcon className="mt-0.5 h-3 w-3 shrink-0 text-muted-foreground" />
									<span className="flex-1 min-w-0">
										<span className="block text-xs text-foreground">
											{t.title}{' '}
											<span className="font-mono text-[10px] text-muted-foreground">
												· {t.id.slice(0, 6)}
											</span>
										</span>
										<span className="block truncate font-mono text-[10px] text-muted-foreground">
											{t.spec.cwd}
											{t.owner.kind === 'studio' && ' · in Studio'}
										</span>
									</span>
								</button>
							</li>
						))}
					</ul>
				)}
			</div>
			{conflict && (
				<AttachmentConflictPopover
					conflict={conflict}
					onReclaim={() => {
						const res = useTerminalStore
							.getState()
							.attachToStudio(conflict.tabId, paneId, artifactPath, { force: true });
						if (res.ok) {
							onAttach(conflict.tabId);
							setConflict(null);
						}
					}}
					onOpenOther={() => {
						usePaneStore.getState().focusPane(conflict.previousPaneId);
						setConflict(null);
					}}
					onDismiss={() => setConflict(null)}
				/>
			)}
		</div>
	);
}

interface AttachmentConflictPopoverProps {
	conflict: { tabId: string; previousPaneId: string };
	onReclaim: () => void;
	onOpenOther: () => void;
	onDismiss: () => void;
}

function AttachmentConflictPopover({
	conflict,
	onReclaim,
	onOpenOther,
	onDismiss,
}: AttachmentConflictPopoverProps) {
	return (
		<div className="absolute inset-x-2 bottom-2 rounded border border-amber-500/60 bg-background p-3 shadow-lg">
			<div className="mb-2 text-xs text-foreground">
				Terminal <span className="font-mono text-[11px]">{conflict.tabId.slice(0, 8)}</span> is
				attached to Studio pane{' '}
				<span className="font-mono text-[11px]">{conflict.previousPaneId.slice(0, 6)}</span>.
			</div>
			<div className="flex items-center justify-end gap-2">
				<Button size="sm" variant="ghost" onClick={onDismiss} className="h-7 px-2 text-[11px]">
					Cancel
				</Button>
				<Button size="sm" variant="outline" onClick={onOpenOther} className="h-7 px-2 text-[11px]">
					<ExternalLink className="mr-1 h-3 w-3" />
					Open that pane
				</Button>
				<Button size="sm" onClick={onReclaim} className="h-7 px-2 text-[11px]">
					Reclaim here
				</Button>
			</div>
		</div>
	);
}

// ─── Notices ─────────────────────────────────────────────────────────

interface StaleAttachmentNoticeProps {
	onClear: () => void;
}

function StaleAttachmentNotice({ onClear }: StaleAttachmentNoticeProps) {
	return (
		<div className="flex h-full w-full flex-col items-center justify-center gap-3 p-6 text-center">
			<div className="font-mono text-[10px] uppercase tracking-[0.14em] text-muted-foreground">
				Stale attachment
			</div>
			<div className="text-sm text-foreground">The attached terminal no longer exists.</div>
			<Button size="sm" onClick={onClear} className="h-7 px-3 text-xs">
				<RefreshCcw className="mr-1 h-3 w-3" />
				Pick another
			</Button>
		</div>
	);
}

interface ExitedAttachmentNoticeProps {
	tab: TerminalTab;
	onDetach: () => void;
}

function ExitedAttachmentNotice({ tab, onDetach }: ExitedAttachmentNoticeProps) {
	return (
		<div className="flex h-full w-full flex-col items-center justify-center gap-3 p-6 text-center">
			<div className="font-mono text-[10px] uppercase tracking-[0.14em] text-muted-foreground">
				Terminal exited
			</div>
			<div className="text-sm text-foreground">
				{tab.title}
				{tab.exitCode !== null ? ` · exit ${tab.exitCode}` : ''}
			</div>
			<Button size="sm" onClick={onDetach} className="h-7 px-3 text-xs">
				<X className="mr-1 h-3 w-3" />
				Detach + pick another
			</Button>
		</div>
	);
}

// ─── Chrome chip ─────────────────────────────────────────────────────

interface TerminalChipProps {
	tabId: string;
	onClick: () => void;
	onDetach: () => void;
}

export function TerminalChip({ tabId, onClick, onDetach }: TerminalChipProps) {
	const tab = useTerminalStore((s) => s.tabs.find((t) => t.id === tabId));
	if (!tab) return null;
	return (
		<div
			className={cn(
				'flex items-center gap-1 rounded border border-amber-500/60 bg-amber-500/10 px-1.5 py-0.5 text-[10px] text-amber-700 dark:text-amber-300'
			)}
		>
			<button
				type="button"
				onClick={onClick}
				className="flex items-center gap-1"
				title={`Attached terminal · ${tab.spec.cwd}`}
			>
				<TerminalIcon className="h-2.5 w-2.5" />
				<span className="font-mono">→ term {tabId.slice(0, 6)}</span>
				<span className="text-muted-foreground">· {tab.title}</span>
			</button>
			<button
				type="button"
				onClick={(e) => {
					e.stopPropagation();
					onDetach();
				}}
				className="ml-0.5 text-muted-foreground hover:text-foreground"
				aria-label="Detach terminal"
				title="Detach"
			>
				<X className="h-2.5 w-2.5" />
			</button>
		</div>
	);
}

// ─── Chrome attach button + popover ──────────────────────────────────

interface StudioTerminalAttachButtonProps {
	paneId: string;
	artifactPath: string;
	onAttach: (tabId: string) => void;
}

/** Sibling of `TerminalChip` for the chrome row when no terminal is
 *  attached. Click reveals a small picker popover anchored to the
 *  button (portalled to <body> to escape the panel's overflow:hidden,
 *  same pattern as the pin-review popover). */
export function StudioTerminalAttachButton({
	paneId,
	artifactPath,
	onAttach,
}: StudioTerminalAttachButtonProps) {
	const [open, setOpen] = useState(false);
	const buttonRef = useRef<HTMLButtonElement | null>(null);
	return (
		<>
			<button
				ref={buttonRef}
				type="button"
				onClick={() => setOpen((v) => !v)}
				aria-label="Attach terminal to this Studio pane"
				title="Attach terminal"
				aria-expanded={open}
				className="inline-flex h-6 items-center gap-1 rounded border border-dashed border-border bg-background px-1.5 py-0.5 text-[10px] text-muted-foreground transition-colors hover:bg-muted/30 hover:text-foreground"
			>
				<TerminalIcon className="h-3 w-3" />
				<span className="font-mono">Attach…</span>
			</button>
			{open && (
				<StudioTerminalAttachPopover
					anchorEl={buttonRef.current}
					paneId={paneId}
					artifactPath={artifactPath}
					onClose={() => setOpen(false)}
					onAttach={(tabId) => {
						setOpen(false);
						onAttach(tabId);
					}}
				/>
			)}
		</>
	);
}

interface StudioTerminalAttachPopoverProps {
	anchorEl: HTMLElement | null;
	paneId: string;
	artifactPath: string;
	onClose: () => void;
	onAttach: (tabId: string) => void;
}

const ATTACH_POPOVER_WIDTH = 320;
const ATTACH_POPOVER_MARGIN = 8;
const ATTACH_POPOVER_EST_HEIGHT = 360;

function StudioTerminalAttachPopover({
	anchorEl,
	paneId,
	artifactPath,
	onClose,
	onAttach,
}: StudioTerminalAttachPopoverProps) {
	const tabs = useTerminalStore((s) => s.tabs);
	const [conflict, setConflict] = useState<{ tabId: string; previousPaneId: string } | null>(null);
	const [pos, setPos] = useState<{ top: number; left: number } | null>(null);

	useEffect(() => {
		if (!anchorEl) {
			setPos(null);
			return;
		}
		const rect = anchorEl.getBoundingClientRect();
		const vw = typeof window !== 'undefined' ? window.innerWidth : 1024;
		const vh = typeof window !== 'undefined' ? window.innerHeight : 768;
		// Anchor below the button; flip up if it would clip the bottom.
		const desiredLeft = rect.right - ATTACH_POPOVER_WIDTH;
		const clampedLeft = Math.max(
			ATTACH_POPOVER_MARGIN,
			Math.min(desiredLeft, vw - ATTACH_POPOVER_WIDTH - ATTACH_POPOVER_MARGIN)
		);
		const belowTop = rect.bottom + 4;
		const flipsUp = belowTop + ATTACH_POPOVER_EST_HEIGHT + ATTACH_POPOVER_MARGIN > vh;
		const top = flipsUp
			? Math.max(ATTACH_POPOVER_MARGIN, rect.top - ATTACH_POPOVER_EST_HEIGHT - 4)
			: belowTop;
		setPos({ top, left: clampedLeft });
	}, [anchorEl]);

	useEffect(() => {
		const onKey = (e: KeyboardEvent) => {
			if (e.key === 'Escape') onClose();
		};
		const onDoc = (e: MouseEvent) => {
			const t = e.target as HTMLElement | null;
			if (t?.closest('[data-studio-terminal-attach-popover]')) return;
			if (t && anchorEl?.contains(t)) return;
			onClose();
		};
		window.addEventListener('keydown', onKey);
		window.addEventListener('mousedown', onDoc, true);
		return () => {
			window.removeEventListener('keydown', onKey);
			window.removeEventListener('mousedown', onDoc, true);
		};
	}, [anchorEl, onClose]);

	const spawnCwd = activeProjectCwd();
	const candidates = useMemo(
		() =>
			tabs.filter(
				(t) =>
					t.status === 'running' &&
					(t.owner.kind === 'sidepane' || (t.owner.kind === 'studio' && t.owner.paneId !== paneId))
			),
		[tabs, paneId]
	);

	const tryAttach = (tabId: string) => {
		const res = useTerminalStore.getState().attachToStudio(tabId, paneId, artifactPath);
		if (res.ok) {
			onAttach(tabId);
			return;
		}
		setConflict({ tabId, previousPaneId: res.previousPaneId });
	};

	const spawnNew = (preset: ShellPreset) => {
		const id = createTerminalSession({ cwd: spawnCwd, cmd: preset.cmd, title: preset.title });
		useTerminalStore.getState().attachToStudio(id, paneId, artifactPath);
		onAttach(id);
	};

	if (!pos) return null;

	return createPortal(
		<div
			data-studio-terminal-attach-popover
			role="dialog"
			aria-label="Attach a terminal"
			className="fixed z-50 flex flex-col rounded border border-border bg-background shadow-xl"
			style={{ top: pos.top, left: pos.left, width: ATTACH_POPOVER_WIDTH }}
		>
			<div className="flex shrink-0 items-center gap-1.5 border-b border-border bg-muted/20 px-2 py-1.5 text-[10px] text-muted-foreground">
				<TerminalIcon className="h-3 w-3" />
				<span className="font-mono">attach a terminal</span>
			</div>
			<div className="border-b border-border px-3 py-2">
				<div className="mb-1.5 flex items-center gap-1 text-[10px] text-muted-foreground">
					<Plus className="h-3 w-3 text-amber-600 dark:text-amber-400" />
					<span className="truncate font-mono" title={spawnCwd}>
						new in {spawnCwd}
					</span>
				</div>
				<div className="flex flex-wrap gap-1">
					{SHELL_PRESETS.map((preset) => (
						<button
							key={preset.title}
							type="button"
							onClick={() => spawnNew(preset)}
							className="rounded border border-border bg-background px-2 py-1 text-[11px] text-foreground transition-colors hover:bg-muted/30"
							title={preset.cmd.join(' ')}
						>
							{preset.label}
						</button>
					))}
				</div>
			</div>
			<div className="max-h-60 min-h-0 overflow-y-auto">
				{candidates.length === 0 ? (
					<div className="px-3 py-6 text-center text-[11px] text-muted-foreground">
						No running terminals to attach.
					</div>
				) : (
					<ul>
						{candidates.map((t) => (
							<li key={t.id}>
								<button
									type="button"
									onClick={() => tryAttach(t.id)}
									className="flex w-full items-start gap-2 border-b border-border px-3 py-2 text-left transition-colors hover:bg-muted/30"
								>
									<TerminalIcon className="mt-0.5 h-3 w-3 shrink-0 text-muted-foreground" />
									<span className="flex-1 min-w-0">
										<span className="block text-xs text-foreground">
											{t.title}{' '}
											<span className="font-mono text-[10px] text-muted-foreground">
												· {t.id.slice(0, 6)}
											</span>
										</span>
										<span className="block truncate font-mono text-[10px] text-muted-foreground">
											{t.spec.cwd}
											{t.owner.kind === 'studio' && ' · in Studio'}
										</span>
									</span>
								</button>
							</li>
						))}
					</ul>
				)}
			</div>
			{conflict && (
				<AttachmentConflictPopover
					conflict={conflict}
					onReclaim={() => {
						const res = useTerminalStore
							.getState()
							.attachToStudio(conflict.tabId, paneId, artifactPath, { force: true });
						if (res.ok) {
							onAttach(conflict.tabId);
							setConflict(null);
						}
					}}
					onOpenOther={() => {
						usePaneStore.getState().focusPane(conflict.previousPaneId);
						setConflict(null);
					}}
					onDismiss={() => setConflict(null)}
				/>
			)}
		</div>,
		document.body
	);
}
