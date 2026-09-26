// Pin composer modal.
//
// Trigger surface: HtmlFrame (full artifact-pane viewer) — opened by the
// element-picker's `contextmenu` capture. The modal collects free-form
// text from the user, shows the cropped element screenshot for context,
// then writes the screenshot to disk, calls `commentCreate`, and asks
// the routing dispatcher to deliver the structured prompt (terminal claude
// if available, otherwise side-pane terminal fallback).
//
// Lives under `shell/src/shell/artifact-studio/` because pins are a
// Studio-surface concept after the unified plan collapsed grid into
// Studio's grid density.

import { useEffect, useRef, useState } from 'react';
import { useQueryClient } from '@tanstack/react-query';
import {
	Dialog,
	DialogContent,
	DialogDescription,
	DialogFooter,
	DialogHeader,
	DialogTitle,
} from '@/components/ui/dialog';
import { Button } from '@/components/ui/button';
import { routeOutcomeLabel, routePin } from '@/lib/artifact/route-pin';
import { focusMarkerProps } from '@/lib/keymap/context-keys';
import { useCommands } from '@/lib/keymap/dispatcher';
import { commentCreate, pinScreenshotWrite } from '@/lib/tauri-cmd';
import {
	readArtifactSink,
	studioSinkToPreferredPtyId,
	studioSinkToRouteOverride,
} from '@/shell/artifact-studio/studio-sink-popover';
import { useTerminalStore } from '@/terminal/session-store';
import type { PickResult } from './element-picker';

/** PTY id of the currently-focused terminal tab, if any. The pin-route
 *  dispatcher uses this as a hint so two concurrent claude PTYs don't race —
 *  the visible terminal wins. */
function activeTerminalPtyId(): string | null {
	const s = useTerminalStore.getState();
	const tab = s.tabs.find((t) => t.id === s.activeId);
	return tab?.ptyId ?? null;
}

interface PinComposerProps {
	open: boolean;
	pick: PickResult | null;
	artifactPath: string;
	/** Tab id of the terminal attached to the Studio pane this composer is
	 *  hosted in, if any. When present, pins route to that PTY regardless of
	 *  the per-artifact sink setting (attachment > sink). The bare HtmlFrame
	 *  consumer leaves this undefined. */
	attachedTerminalId?: string;
	/** Called when the modal closes. `committed` is true when the close
	 *  happened after a successful pin submit, false on cancel / dismiss /
	 *  Escape. Used by the Studio loupe to differentiate "pin landed, drop
	 *  comment-mode" from "user backed out, keep comment-mode armed". */
	onClose: (committed: boolean) => void;
}

export function PinComposer({
	open,
	pick,
	artifactPath,
	attachedTerminalId,
	onClose,
}: PinComposerProps) {
	const qc = useQueryClient();
	const [text, setText] = useState('');
	const [busy, setBusy] = useState(false);
	const [error, setError] = useState<string | null>(null);
	const textareaRef = useRef<HTMLTextAreaElement | null>(null);

	// Reset state every time we open with a fresh pick. Closing keeps the
	// text around for a moment so the fade-out doesn't show empty content.
	useEffect(() => {
		if (open && pick) {
			setText('');
			setError(null);
			// Defer focus until the Radix portal finishes mounting.
			queueMicrotask(() => textareaRef.current?.focus());
		}
	}, [open, pick]);

	const submit = async () => {
		if (!pick) return;
		const trimmed = text.trim();
		if (!trimmed) {
			setError('Comment text is required.');
			return;
		}
		setBusy(true);
		setError(null);
		try {
			const screenshotPath = await pinScreenshotWrite(pick.screenshotBase64);
			const created = await commentCreate({
				artifactPath,
				selector: pick.selector,
				text: trimmed,
				screenshotPath,
				positionX: pick.positionX,
				positionY: pick.positionY,
			});
			// Resolve the per-artifact sink override (if any) and translate
			// it to the Rust `RouteSink`. `inherit` / `auto` / `studio` all
			// resolve to `undefined`, letting the dispatcher run its
			// PTY-detection fallback. Fire-and-forget routing: the
			// dispatcher logs its own errors and the pin itself is already
			// persisted, so we close the modal even if routing chokes.
			let overrideSink: ReturnType<typeof studioSinkToRouteOverride> = undefined;
			let preferredPtyId: string | null = null;
			if (attachedTerminalId) {
				// Embedded-terminal mode wins over the sink popover choice.
				const ts = useTerminalStore.getState();
				preferredPtyId = ts.tabs.find((t) => t.id === attachedTerminalId)?.ptyId ?? null;
				overrideSink = 'terminal';
			} else {
				const sink = await readArtifactSink(artifactPath);
				overrideSink = studioSinkToRouteOverride(sink);
				// Prefer the PTY id encoded in the sink (terminal:<id>) over the
				// focused-tab fallback. The Rust dispatcher validates the hint
				// against the live PTY snapshot and falls back if it's gone.
				preferredPtyId = studioSinkToPreferredPtyId(sink) ?? activeTerminalPtyId();
			}
			void routePin({ id: created.id, overrideSink, preferredPtyId })
				.then((res) => console.info('[pin-composer] pin routed:', routeOutcomeLabel(res)))
				.catch((e) => console.error('[pin-composer] route failed', e));
			// Bust the grid's pins query so the new pin pops in immediately.
			void qc.invalidateQueries({ queryKey: ['artifact-grid'] });
			onClose(true);
		} catch (e) {
			setError(e instanceof Error ? e.message : String(e));
		} finally {
			setBusy(false);
		}
	};

	// WP-56 (G-ACTIONS §10.2/§10.6): migrated from a local ⌘↵ `onKeyDown` on
	// the textarea to the registry `studio.pin-submit` command, scoped by the
	// new `pinComposerFocus` key (B-21). Radix unmounts the textarea (and so
	// the marker) while the dialog is closed, so this never fires then.
	useCommands({ 'studio.pin-submit': () => void submit() });

	return (
		<Dialog
			open={open && pick !== null}
			onOpenChange={(o) => {
				if (!o && !busy) onClose(false);
			}}
		>
			<DialogContent className="sm:max-w-lg">
				<DialogHeader>
					<DialogTitle>Add pin</DialogTitle>
					<DialogDescription>
						{pick ? (
							<span className="font-mono text-[11px] text-muted-foreground">
								{pick.elementLabel} · <span className="opacity-70">{pick.selector}</span>
							</span>
						) : null}
					</DialogDescription>
				</DialogHeader>

				{pick && (
					<div className="mb-2 flex justify-center rounded border border-border bg-muted/30 p-2">
						<img
							src={`data:image/png;base64,${pick.screenshotBase64}`}
							alt="Pinned element"
							className="max-h-48 max-w-full object-contain"
							style={{
								width: pick.screenshotWidth / (window.devicePixelRatio || 1),
								height: pick.screenshotHeight / (window.devicePixelRatio || 1),
							}}
						/>
					</div>
				)}

				<textarea
					ref={textareaRef}
					value={text}
					onChange={(e) => setText(e.target.value)}
					placeholder="What needs to change here?"
					rows={4}
					disabled={busy}
					className="w-full resize-none rounded border border-input bg-background px-3 py-2 text-sm focus:outline-none focus:ring-2 focus:ring-ring"
					{...focusMarkerProps('pin-composer')}
				/>

				{error && <p className="text-xs text-destructive">{error}</p>}

				<DialogFooter>
					<Button variant="ghost" disabled={busy} onClick={() => onClose(false)}>
						Cancel
					</Button>
					<Button disabled={busy || !text.trim()} onClick={() => void submit()}>
						{busy ? 'Saving…' : 'Add pin'}
					</Button>
				</DialogFooter>
				<p className="text-right text-[10px] text-muted-foreground">⌘↵ to submit</p>
			</DialogContent>
		</Dialog>
	);
}
