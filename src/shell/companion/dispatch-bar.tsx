// Dispatch bar — spec §3.10 #62–#65, §5.3. The Companion's ONE text input
// (ADR-021): it sends to the resolved target and clears. It never renders a
// response — output lives in the terminal pane.

import { SendHorizontal } from 'lucide-react';
import { useEffect, useRef, useState } from 'react';
import { focusMarkerProps } from '@/lib/keymap/context-keys';
import { resolveHostedKeypress } from '@/lib/keymap/dispatcher';
import { labelFor } from '@/lib/keymap/registry';
import { type CompanionTarget, useShellStore } from '@/lib/shell/shell-store';
import { useCompanionStore } from './companion-store';
import { currentDispatchContext, resolveTarget } from './resolve-target';
import { TargetPicker } from './target-picker';

/** Below this, a send shows no loading state at all (spec §1.2: no flash). */
const LOADING_DELAY_MS = 150;

// Per-target recall stacks (`↑` recalls the previous dispatch). Session-only.
const recall = new Map<string, string[]>();

function targetKey(t: CompanionTarget): string {
	return t.kind === 'session' ? `session:${t.session_id}` : `${t.kind}:${t.engine_id ?? ''}`;
}

/** Hosted command → dispatch mode (§4.6). */
const DISPATCH_MODE: Readonly<Record<string, 'target' | 'new' | 'persistent'>> = {
	'companion.send': 'target',
	'companion.new-run': 'new',
	'companion.persistent-run': 'persistent',
};

export function DispatchBar() {
	const target = useShellStore((s) => s.companion.activeTarget);
	// Subscribed so a newly-chosen default engine re-enables the input.
	useShellStore((s) => s.defaultEngineId);
	const draft = useCompanionStore((s) => s.draft);
	const setDraft = useCompanionStore((s) => s.setDraft);
	const focusPending = useCompanionStore((s) => s.focusPending);
	const consumeFocus = useCompanionStore((s) => s.consumeFocus);
	const inputRef = useRef<HTMLInputElement | null>(null);
	const rootRef = useRef<HTMLDivElement | null>(null);
	const [busy, setBusy] = useState(false);
	const [error, setError] = useState<string | null>(null);

	const resolved = resolveTarget(target);
	const disabledReason = resolved.kind === 'none' ? resolved.disabledReason : undefined;

	// ⌘2 / ⌘J-expand / "Hand to Chi" / `ikenga:companion-focus` → focus here.
	useEffect(() => {
		if (!focusPending) return;
		consumeFocus();
		const el = inputRef.current;
		if (!el) return;
		if (el.disabled) {
			// Nothing resolves (no engine): a disabled input cannot take focus, so
			// land on the target chip — the control that can fix it.
			rootRef.current?.querySelector<HTMLElement>('[aria-haspopup="menu"]')?.focus();
			return;
		}
		el.focus();
		const end = el.value.length;
		el.setSelectionRange?.(end, end);
	}, [focusPending, consumeFocus]);

	async function dispatch(mode: 'target' | 'new' | 'persistent') {
		const text = draft.trim();
		if (!text || busy) return;
		const engineId = target.kind === 'session' ? null : target.engine_id;
		const effective: CompanionTarget =
			mode === 'target'
				? target
				: mode === 'new'
					? { kind: 'new', engine_id: engineId }
					: { kind: 'persistent', engine_id: engineId };
		const r = resolveTarget(effective);
		if (r.kind === 'none') {
			setError(r.disabledReason ?? 'Nothing to send to');
			return;
		}
		setError(null);
		const timer = setTimeout(() => setBusy(true), LOADING_DELAY_MS);
		try {
			// Fire-and-forget by contract: awaited only to know the host took it.
			await r.send(text, currentDispatchContext());
			const key = targetKey(effective);
			recall.set(key, [...(recall.get(key) ?? []), text].slice(-20));
			setDraft('');
		} catch (e) {
			setError(e instanceof Error ? e.message : String(e));
		} finally {
			clearTimeout(timer);
			setBusy(false);
		}
	}

	function onKeyDown(e: React.KeyboardEvent<HTMLInputElement>) {
		// The three dispatch keys are hosted registry commands (G-ACTIONS §4.6,
		// A-2): `companion.send` / `new-run` / `persistent-run`, `when:
		// dispatchFocus`. Their keys come from the effective keymap (so a
		// rebind in `keybindings.json` changes what sends), and this input —
		// their owner — fires them; the frame dispatcher never does, and
		// leaves a keystroke they claim alone.
		const hosted = resolveHostedKeypress(e.nativeEvent, 'dispatch');
		const mode = hosted ? DISPATCH_MODE[hosted.command] : undefined;
		if (mode) {
			e.preventDefault();
			void dispatch(mode);
			return;
		}
		if (e.key === 'ArrowUp' && !draft) {
			const stack = recall.get(targetKey(target));
			const prev = stack?.[stack.length - 1];
			if (prev) {
				e.preventDefault();
				setDraft(prev);
			}
			return;
		}
		if (e.key === 'Escape') {
			// Blur, never clear (spec §3.10 #63).
			e.preventDefault();
			inputRef.current?.blur();
		}
	}

	const sendHint = labelFor('companion.send');
	const newRunHint = labelFor('companion.new-run');
	const persistentHint = labelFor('companion.persistent-run');

	return (
		<div
			ref={rootRef}
			className="shrink-0 border-b px-3 pb-3 pt-2"
			style={{ borderColor: 'var(--border)' }}
		>
			<TargetPicker />
			<div
				className="flex h-8 items-center gap-2 rounded-md border pl-3 pr-1 focus-within:border-[var(--primary)]"
				style={{ background: 'var(--bg-sunken)', borderColor: 'var(--border-strong)' }}
				aria-busy={busy || undefined}
			>
				<input
					ref={inputRef}
					type="text"
					value={draft}
					onChange={(e) => setDraft(e.target.value)}
					onKeyDown={onKeyDown}
					disabled={Boolean(disabledReason)}
					title={disabledReason}
					placeholder="Dispatch an instruction…"
					aria-label="Dispatch an instruction"
					data-companion-dispatch=""
					{...focusMarkerProps('dispatch')}
					className="min-w-0 flex-1 bg-transparent text-[13px] outline-none placeholder:text-[var(--fg-muted)] disabled:cursor-not-allowed"
					style={{ color: 'var(--fg)' }}
				/>
				<button
					type="button"
					onClick={() => void dispatch('target')}
					disabled={!draft.trim() || Boolean(disabledReason) || busy}
					title={
						disabledReason ?? (draft.trim() ? `Send (${sendHint})` : 'Type an instruction to send')
					}
					aria-label="Send"
					className="grid size-6 shrink-0 place-items-center rounded-sm bg-[var(--primary)] text-[var(--primary-fg)] focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring disabled:bg-[var(--bg-raised)] disabled:text-[var(--fg-faint)]"
				>
					<SendHorizontal className="h-3.5 w-3.5" aria-hidden="true" />
				</button>
			</div>
			<div className="mt-1 flex flex-wrap gap-x-3 text-[11px]" style={{ color: 'var(--fg-muted)' }}>
				<span>
					<kbd className="font-mono">{sendHint}</kbd> send
				</span>
				<span>
					<kbd className="font-mono">{newRunHint}</kbd> new run
				</span>
				<span>
					<kbd className="font-mono">{persistentHint}</kbd> persistent run
				</span>
			</div>
			{error && (
				<p role="alert" className="mt-1 text-[11px]" style={{ color: 'var(--color-text-danger)' }}>
					{error}
				</p>
			)}
		</div>
	);
}
