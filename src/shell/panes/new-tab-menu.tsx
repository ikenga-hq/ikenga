import { Command } from 'cmdk';
import { Hash, Terminal as TerminalIcon } from 'lucide-react';
import { useEffect, useState } from 'react';
import { CommandRow, type CommandRowProps } from '@/components/ui/command-row';
import { usePaneStore } from '@/lib/panes/pane-store';
import type { LeafNode, PaneView } from '@/lib/panes/types';
import { useDefaultShellProfile } from '@/lib/shell-profiles';
import { NAV_GROUPS } from '@/shell/nav-config';
import { createClaudeTerminalSession, createTerminalSession } from '@/terminal/single-terminal';

interface NewTabMenuProps {
	leaf: LeafNode;
	open: boolean;
	onClose: () => void;
	anchor: { top: number; left: number } | null;
}

export function NewTabMenu({ leaf, open, onClose, anchor }: NewTabMenuProps) {
	const addTab = usePaneStore((s) => s.addTab);
	const focusPane = usePaneStore((s) => s.focusPane);
	const { profiles, selectedProfile } = useDefaultShellProfile();

	// Click outside to close. Defer one tick so the click that opened the
	// menu isn't itself caught here.
	useEffect(() => {
		if (!open) return;
		const onDown = () => onClose();
		const onKey = (e: KeyboardEvent) => {
			if (e.key === 'Escape') onClose();
		};
		const t = setTimeout(() => {
			window.addEventListener('mousedown', onDown);
			window.addEventListener('keydown', onKey);
		}, 0);
		return () => {
			clearTimeout(t);
			window.removeEventListener('mousedown', onDown);
			window.removeEventListener('keydown', onKey);
		};
	}, [open, onClose]);

	const [search, setSearch] = useState('');

	if (!open || !anchor) return null;

	function commit(view: PaneView) {
		focusPane(leaf.id);
		addTab(leaf.id, view);
		onClose();
	}

	return (
		<div
			onMouseDown={(e) => e.stopPropagation()}
			className="fixed z-50 w-80 overflow-hidden rounded-md border border-border bg-popover text-popover-foreground shadow-lg"
			style={{ top: anchor.top, left: anchor.left }}
		>
			<Command label="New tab" className="flex flex-col">
				<Command.Input
					autoFocus
					value={search}
					onValueChange={setSearch}
					placeholder="Open…"
					className="w-full border-b border-border bg-transparent px-3 py-2 text-xs outline-none placeholder:text-muted-foreground"
				/>
				<Command.List className="max-h-72 overflow-y-auto p-1">
					<Command.Empty className="py-4 text-center text-xs text-muted-foreground">
						No matches.
					</Command.Empty>

					<Command.Group
						heading="Terminals & Shells"
						className="text-[10px] uppercase tracking-wider text-muted-foreground"
					>
						<MenuItem
							onSelect={() =>
								commit({
									kind: 'terminal',
									sessionId: createTerminalSession({
										cmd: selectedProfile.cmd,
										title: selectedProfile.label,
									}),
								})
							}
							Icon={TerminalIcon}
							label={`Terminal (${selectedProfile.label})`}
							shortcut="⌘T"
						/>
						{profiles
							.filter((p) => p.id !== selectedProfile.id)
							.map((p) => (
								<MenuItem
									key={p.id}
									onSelect={() =>
										commit({
											kind: 'terminal',
											sessionId: createTerminalSession({
												cmd: p.cmd,
												title: p.label,
											}),
										})
									}
									Icon={TerminalIcon}
									label={p.label}
									detail={p.cmd.join(' ')}
								/>
							))}
						<MenuItem
							onSelect={() =>
								commit({
									kind: 'terminal',
									sessionId: createClaudeTerminalSession(),
								})
							}
							Icon={TerminalIcon}
							label="Claude terminal"
							shortcut="⌘⇧T"
						/>
						{profiles
							.filter((p) => p.kind === 'wsl')
							.map((p) => (
								<MenuItem
									key={`claude-${p.id}`}
									onSelect={() =>
										commit({
											kind: 'terminal',
											sessionId: createClaudeTerminalSession({
												shellTarget: 'wsl',
												wslDistro: p.distro,
											}),
										})
									}
									Icon={TerminalIcon}
									label={`Claude (${p.label})`}
								/>
							))}
					</Command.Group>

					<Command.Group
						heading="Routes"
						className="text-[10px] uppercase tracking-wider text-muted-foreground"
					>
						{NAV_GROUPS.flatMap((g) => g.items).map(({ to, label, Icon }) => (
							<MenuItem
								key={to}
								onSelect={() => commit({ kind: 'route', path: to })}
								Icon={Icon}
								label={label}
								detail={to}
							/>
						))}
					</Command.Group>

					<Command.Group
						heading="Files"
						className="text-[10px] uppercase tracking-wider text-muted-foreground"
					>
						<Command.Item
							onSelect={() => onClose()}
							className="flex cursor-default items-center gap-2 rounded-md px-2 py-1.5 text-xs text-muted-foreground"
						>
							<Hash className="h-3.5 w-3.5" />
							<span>Open via Files mode in the sidebar</span>
						</Command.Item>
					</Command.Group>
				</Command.List>
			</Command>
		</div>
	);
}

// New-tab rows share the consolidated `CommandRow` (size `sm`); thin alias
// binds the size so the call sites above stay terse.
function MenuItem(props: Omit<CommandRowProps, 'size' | 'as'>) {
	return <CommandRow size="sm" {...props} />;
}

export function useAnchorRect(open: boolean, ref: React.RefObject<HTMLElement | null>) {
	const [rect, setRect] = useState<{ top: number; left: number } | null>(null);
	useEffect(() => {
		if (!open) {
			setRect(null);
			return;
		}
		if (!ref.current) return;
		const r = ref.current.getBoundingClientRect();
		setRect({ top: r.bottom + 2, left: Math.max(r.left - 240, 8) });
	}, [open, ref]);
	return rect;
}
