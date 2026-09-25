// Step 6 (D-04 `shortcuts`) — the five keys that matter. `new` — no shipped
// equivalent. Per `drafts/design-spec-D-03-07.md` §D-04: "the five keys that
// matter (⌘1 ⌘2 ⌘3 ⌘K ⌘P), platform-aware (Ctrl on Windows)".
//
// Purely informational — nothing is written here (the keymap registry
// itself, `src/lib/keymap`, is a Phase 6 / D-06 deliverable per the shell
// CLAUDE.md; rebinding isn't possible yet, so this step can't offer it).

import { useState } from 'react';

import { Button } from '@/components/ui/button';
import { cn } from '@/components/ui/utils';
import { WritesNote } from '@/shell/onboarding/footer';

interface ShortcutsBodyProps {
	onContinue: () => void;
}

type Platform = 'mac' | 'win';

function detectPlatform(): Platform {
	if (typeof navigator === 'undefined') return 'win';
	const ua = `${navigator.platform ?? ''} ${navigator.userAgent ?? ''}`;
	return /mac/i.test(ua) ? 'mac' : 'win';
}

const KEYS: ReadonlyArray<{ mac: string; win: string; nm: string; sub: string }> = [
	{ mac: '⌘1', win: 'Ctrl 1', nm: 'Project', sub: 'The Explorer for the active project.' },
	{ mac: '⌘2', win: 'Ctrl 2', nm: 'Chi', sub: 'The Companion: dispatch, permissions, cost.' },
	{ mac: '⌘3', win: 'Ctrl 3', nm: 'Ngwa', sub: 'One catalogue of everything you wield.' },
	{ mac: '⌘K', win: 'Ctrl K', nm: 'Command palette', sub: 'Every action, and every key, in one list.' },
	{ mac: '⌘P', win: 'Ctrl P', nm: 'Switch project', sub: 'Change the container everything is scoped to.' },
];

export function ShortcutsBody({ onContinue }: ShortcutsBodyProps) {
	const [platform, setPlatform] = useState<Platform>(() => detectPlatform());

	return (
		<div className="mx-auto max-w-4xl">
			<div className="mb-8">
				<p
					className="mb-2 text-xs font-semibold uppercase tracking-[0.04em]"
					style={{ color: 'var(--primary)' }}
				>
					Five keys
				</p>
				<h1 className="font-display text-3xl font-bold leading-tight tracking-tight">
					Five keys.
				</h1>
				<p className="mt-2 max-w-[60ch] text-sm" style={{ color: 'var(--fg-muted)' }}>
					The rest live in the command palette, and every one of them can be rebound later.
					Nothing here is written unless you change something.
				</p>
			</div>

			<div
				className="mb-6 inline-flex w-fit items-center gap-1 rounded-md border p-1 font-mono text-xs"
				style={{ borderColor: 'var(--border-soft)', background: 'var(--bg-surface)' }}
				role="group"
				aria-label="Platform"
			>
				{(['win', 'mac'] as const).map((p) => (
					<button
						key={p}
						type="button"
						onClick={() => setPlatform(p)}
						aria-pressed={platform === p}
						data-testid={`shortcuts-plat-${p}`}
						className={cn('h-7 rounded-sm px-3 font-medium')}
						style={{
							background: platform === p ? 'var(--bg-base)' : 'transparent',
							fontWeight: platform === p ? 600 : 500,
						}}
					>
						{p === 'mac' ? 'macOS' : 'Windows'}
					</button>
				))}
			</div>

			<div className="grid gap-3 sm:grid-cols-2" data-testid="shortcuts-keys">
				{KEYS.map((k) => (
					<div
						key={k.nm}
						className="flex items-center gap-4 rounded-lg border p-4"
						style={{ borderColor: 'var(--border-soft)', background: 'var(--bg-surface)' }}
					>
						<kbd
							className="flex h-9 min-w-[52px] items-center justify-center rounded-md border px-2 font-mono text-xs font-semibold"
							style={{ borderColor: 'var(--border-strong)', background: 'var(--bg-raised)' }}
						>
							{platform === 'mac' ? k.mac : k.win}
						</kbd>
						<div className="min-w-0">
							<div className="text-[13px] font-semibold">{k.nm}</div>
							<div className="mt-0.5 text-[11.5px]" style={{ color: 'var(--fg-muted)' }}>
								{k.sub}
							</div>
						</div>
					</div>
				))}
			</div>

			<div
				className="mt-6 rounded-md border p-4 text-xs"
				style={{ borderColor: 'var(--border-soft)', color: 'var(--fg-muted)' }}
			>
				<div className="flex items-center justify-between" style={{ padding: 0 }}>
					<span>Rebind any of them</span>
					<span className="font-mono">Settings › Keys (coming soon)</span>
				</div>
				<p className="mt-2">
					The palette also lists every key, grouped, and filters as you type. There's no separate
					keymap editor yet — that ships with a later Actions &amp; keys phase.
				</p>
			</div>

			<WritesNote stepId="shortcuts" />

			<div className="mt-8 flex items-center justify-end gap-3">
				<Button onClick={onContinue} data-testid="shortcuts-inline-continue">
					Continue
				</Button>
			</div>
		</div>
	);
}
