import { RefreshCw, SplitSquareHorizontal, SplitSquareVertical, X } from 'lucide-react';
import type { PaneId, PaneView } from '@/lib/panes/types';
import { usePaneStore } from '@/lib/panes/pane-store';
import { findLeaf } from '@/lib/panes/pane-reducer';
import { IconButton } from '@/components/ui/icon-button';
import { useWebviewRoute } from './pane-views';
import { Popover, PopoverTrigger, PopoverContent } from '@/components/ui/popover';
import { pkgWebviewClearSession } from '@/lib/tauri-cmd';
import { useState } from 'react';
import { cn } from '@/components/ui/utils';

interface PaneToolbarProps {
	paneId: PaneId;
}

export function PaneToolbar({ paneId }: PaneToolbarProps) {
	const splitPane = usePaneStore((s) => s.splitPane);
	const closePane = usePaneStore((s) => s.closePane);
	const refreshPane = usePaneStore((s) => s.refreshPane);
	const canSplit = usePaneStore((s) => s.canSplit());
	const leafCount = usePaneStore((s) => s.leafCount());
	const activeTab = usePaneStore((s) => {
		const leaf = findLeaf(s.root, paneId);
		return leaf?.tabs[leaf.activeTabIdx];
	});

	const splitDisabled = !canSplit;
	const splitTitle = splitDisabled ? 'Max 6 panes' : undefined;
	const closeDisabled = leafCount <= 1;

	return (
		<div className="flex items-center gap-0.5">
			<WebviewSessionControl view={activeTab} paneId={paneId} />
			<IconButton
				onClick={() => refreshPane(paneId)}
				title="Refresh pane content"
				aria-label="Refresh pane"
			>
				<RefreshCw className="h-3.5 w-3.5" />
			</IconButton>
			<IconButton
				onClick={() => splitPane(paneId, 'horizontal')}
				disabled={splitDisabled}
				title={splitTitle ?? 'Split right (⌘\\)'}
				aria-label="Split right"
			>
				<SplitSquareHorizontal className="h-3.5 w-3.5" />
			</IconButton>
			<IconButton
				onClick={() => splitPane(paneId, 'vertical')}
				disabled={splitDisabled}
				title={splitTitle ?? 'Split down (⌘⇧\\)'}
				aria-label="Split down"
			>
				<SplitSquareVertical className="h-3.5 w-3.5" />
			</IconButton>
			<IconButton
				onClick={() => closePane(paneId)}
				disabled={closeDisabled}
				title={closeDisabled ? 'Cannot close last pane' : 'Close pane'}
				aria-label="Close pane"
			>
				<X className="h-3.5 w-3.5" />
			</IconButton>
		</div>
	);
}

function WebviewSessionControl({ view, paneId }: { view: PaneView | undefined; paneId: PaneId }) {
	const webviewEntry = useWebviewRoute(view);
	const [isOpen, setIsOpen] = useState(false);
	const [persistence, setPersistence] = useState<'keep' | 'clear-on-exit' | 'ask'>('ask');

	if (!webviewEntry) return null;

	const handleClearSession = async () => {
		setIsOpen(false);
		await pkgWebviewClearSession(webviewEntry.pkg_id, paneId);
	};

	return (
		<Popover open={isOpen} onOpenChange={setIsOpen}>
			<PopoverTrigger asChild>
				<button
					title={`Session: ${persistence}`}
					aria-label={`Session: ${persistence}`}
					className={cn(
						"relative flex h-[26px] w-[26px] items-center justify-center rounded-[4px] text-muted-foreground transition-all hover:bg-muted hover:text-foreground",
						isOpen && "bg-muted text-foreground"
					)}
				>
					{persistence === 'keep' && (
						<svg viewBox="0 0 24 24" aria-hidden="true" className="h-[14px] w-[14px] fill-none stroke-current stroke-[1.75px] [stroke-linecap:round] [stroke-linejoin:round]">
							<path d="M6 8h12v9a4 4 0 0 1-4 4h-4a4 4 0 0 1-4-4z" />
							<path d="M5 5h14v3H5z" />
							<path d="M9 13h6" />
						</svg>
					)}
					{persistence === 'clear-on-exit' && (
						<svg viewBox="0 0 24 24" aria-hidden="true" className="h-[14px] w-[14px] fill-none stroke-current stroke-[1.75px] [stroke-linecap:round] [stroke-linejoin:round]">
							<path d="M6 8h12v9a4 4 0 0 1-4 4h-4a4 4 0 0 1-4-4z" strokeDasharray="2.5 2.5" />
							<path d="M5 5h14v3H5z" />
							<path d="m9.5 12.5 5 5" />
							<path d="m14.5 12.5-5 5" />
						</svg>
					)}
					{persistence === 'ask' && (
						<svg viewBox="0 0 24 24" aria-hidden="true" className="h-[14px] w-[14px] fill-none stroke-current stroke-[1.75px] [stroke-linecap:round] [stroke-linejoin:round]">
							<path d="M6 8h12v9a4 4 0 0 1-4 4h-4a4 4 0 0 1-4-4z" />
							<path d="M5 5h14v3H5z" />
							<path d="M10.4 13.2a1.7 1.7 0 1 1 2.1 1.9v1.1" />
							<path d="M12.5 18.2h.01" />
						</svg>
					)}
					<span
						className={cn(
							"absolute bottom-[1px] right-[1px] h-[7px] w-[7px] rounded-full border-[1.5px] border-background",
							persistence === 'keep' && "bg-[#30a46c]", // var(--live) equivalent
							persistence === 'clear-on-exit' && "bg-[#d97706]", // var(--achievement) equivalent
							persistence === 'ask' && "bg-[#3b82f6]" // var(--info) equivalent
						)}
					/>
				</button>
			</PopoverTrigger>
			<PopoverContent align="end" className="w-72 p-4">
				<h4 className="mb-1 text-[11.5px] font-semibold">Session</h4>
				<div className="mb-3 font-mono text-[11px] text-muted-foreground break-all">{webviewEntry.pkg_id}</div>
				
				<div className="flex flex-col gap-1" role="radiogroup">
					<PersistenceOption
						selected={persistence === 'keep'}
						onClick={() => setPersistence('keep')}
						title="Keep signed in"
						desc="Cookies persist across restarts."
						colorClass="border-[#30a46c] after:bg-[#30a46c]"
					/>
					<PersistenceOption
						selected={persistence === 'clear-on-exit'}
						onClick={() => setPersistence('clear-on-exit')}
						title="Clear on exit"
						desc="Jar is wiped when Ikenga quits."
						colorClass="border-[#d97706] after:bg-[#d97706]"
					/>
					<PersistenceOption
						selected={persistence === 'ask'}
						onClick={() => setPersistence('ask')}
						title="Ask each time"
						desc="Prompt on first open of a session."
						colorClass="border-[#3b82f6] after:bg-[#3b82f6]"
					/>
				</div>
				
				<div className="my-3 -mx-4 h-px bg-border" />
				
				<button
					onClick={handleClearSession}
					className="flex h-8 w-full items-center justify-center gap-2 rounded-md border border-destructive/20 bg-destructive/10 text-[11.5px] font-semibold text-destructive transition-colors hover:border-destructive hover:bg-destructive hover:text-destructive-foreground"
				>
					<svg viewBox="0 0 24 24" className="h-[13px] w-[13px] fill-none stroke-current stroke-[1.75px] [stroke-linecap:round] [stroke-linejoin:round]">
						<path d="M3 6h18" />
						<path d="M8 6V4h8v2" />
						<path d="M19 6l-1 14H6L5 6" />
					</svg>
					Clear session now
				</button>
				<p className="mt-2 text-[11px] leading-tight text-muted-foreground">
					Wipes <code>webjars/{webviewEntry.pkg_id}/default/</code> after the webview is destroyed. Forces re-login.
				</p>
			</PopoverContent>
		</Popover>
	);
}

function PersistenceOption({
	selected,
	onClick,
	title,
	desc,
	colorClass,
}: {
	selected: boolean;
	onClick: () => void;
	title: string;
	desc: string;
	colorClass: string;
}) {
	return (
		<button
			role="radio"
			aria-checked={selected}
			onClick={onClick}
			className={cn(
				"flex w-full cursor-pointer gap-3 rounded-md border border-transparent p-2 px-3 text-left transition-colors hover:bg-muted",
				selected && "border-border bg-muted"
			)}
		>
			<span
				className={cn(
					"mt-[3px] flex h-[13px] w-[13px] shrink-0 items-center justify-center rounded-full border-[1.5px] border-muted-foreground/30 transition-colors after:h-1.5 after:w-1.5 after:rounded-full after:content-['']",
					selected && colorClass
				)}
			/>
			<div>
				<b className="block text-[11.5px] font-semibold">{title}</b>
				<small className="mt-[1px] block text-[11px] leading-tight text-muted-foreground">
					{desc}
				</small>
			</div>
		</button>
	);
}

