// Step 6 (D-04 `shortcuts`) — the five keys that matter. `new` — no shipped
// equivalent. Per `drafts/design-spec-D-03-07.md` §D-04: "the five keys that
// matter (⌘1 ⌘2 ⌘3 ⌘K ⌘P), platform-aware (Ctrl on Windows)". Layout follows
// `designs/onboarding.html` `VIEW.shortcuts`: "The five" (col A) beside
// "Everything else" — palette, shortcut list, rebinding, and a "Show the
// palette" action (col B); platform preview switch in the header.
//
// Every key label comes from the WP-08 keymap registry (`labelFor`), never a
// literal, so a rebinding shows up here the day the registry learns one.
// Purely informational — nothing is written.

import { Command } from 'lucide-react';
import { useState } from 'react';

import { Button } from '@/components/ui/button';
import { cn } from '@/components/ui/utils';
import { isMacPlatform } from '@/lib/keymap/platform';
import { labelFor } from '@/lib/keymap/registry';
import { CommandPalette } from '@/shell/command-palette';
import { WritesNote } from '@/shell/onboarding/footer';

interface ShortcutsBodyProps {
	onContinue: () => void;
}

type Platform = 'mac' | 'win';

const KEYS: ReadonlyArray<{ command: string; nm: string; sub: string }> = [
	{ command: 'rail.project', nm: 'Project', sub: 'The Explorer for the active project.' },
	{ command: 'rail.chi', nm: 'Chi', sub: 'The Companion: dispatch, permissions, cost.' },
	{ command: 'rail.ngwa', nm: 'Ngwa', sub: 'One catalogue of everything you wield.' },
	{ command: 'palette.open', nm: 'Command palette', sub: 'Every action, and every key, in one list.' },
	{
		command: 'palette.projects',
		nm: 'Switch project',
		sub: 'Change the container everything is scoped to.',
	},
];

export function ShortcutsBody({ onContinue }: ShortcutsBodyProps) {
	const [detected] = useState<Platform>(() => (isMacPlatform() ? 'mac' : 'win'));
	const [platform, setPlatform] = useState<Platform>(detected);
	const [paletteOpen, setPaletteOpen] = useState(false);
	const mac = platform === 'mac';
	const key = (command: string) => labelFor(command, { mac });

	return (
		<div className="mx-auto max-w-6xl">
			<div className="mb-8 flex items-end justify-between gap-6">
				<div>
					<h1 className="font-display text-3xl font-bold leading-tight tracking-tight">
						Five keys.
					</h1>
					<p className="mt-2 max-w-[60ch] text-sm" style={{ color: 'var(--fg-muted)' }}>
						The rest live in the command palette, and every one of them can be rebound. Nothing
						here is written unless you change something.
					</p>
				</div>
				<div
					className="inline-flex w-fit flex-none items-center gap-0.5 rounded-md border border-border p-0.5 font-mono text-xs"
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
							className={cn(
								'min-h-[var(--tab-h)] rounded px-2 py-1 transition-colors',
								platform === p
									? 'bg-card text-foreground shadow-sm'
									: 'text-muted-foreground hover:text-foreground'
							)}
						>
							{p === 'mac' ? 'macOS' : 'Windows'}
						</button>
					))}
				</div>
			</div>

			<div className="grid gap-10 lg:grid-cols-[1.4fr_1fr]">
				{/* ── Col A: the five ─────────────────────────────────── */}
				<div>
					<div
						className="overflow-hidden rounded-md border"
						style={{ borderColor: 'var(--border)', background: 'var(--bg-surface)' }}
					>
						<div
							className="flex items-center gap-2 border-b px-3 py-2 text-[11px] font-semibold uppercase tracking-[0.06em]"
							style={{ borderColor: 'var(--border-soft)', color: 'var(--fg-muted)' }}
						>
							<span>The five</span>
							<span
								className="ml-auto font-mono normal-case tracking-normal"
								style={{ color: 'var(--fg-faint)' }}
							>
								{platform === detected ? 'this machine' : 'preview'} ·{' '}
								{mac ? 'macOS' : 'Windows'}
							</span>
						</div>
						<div className="grid gap-3 p-3 sm:grid-cols-2" data-testid="shortcuts-keys">
							{KEYS.map((k) => (
								<div
									key={k.command}
									className="flex items-center gap-4 rounded-lg border p-4"
									style={{ borderColor: 'var(--border-soft)', background: 'var(--bg-base)' }}
								>
									<kbd
										className="flex h-9 min-w-[52px] items-center justify-center rounded-md border px-2 font-mono text-xs font-semibold"
										style={{ borderColor: 'var(--border-strong)', background: 'var(--bg-raised)' }}
									>
										{key(k.command)}
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
							className="border-t px-3 py-2 text-[11px]"
							style={{ borderColor: 'var(--border-soft)', color: 'var(--fg-faint)' }}
						>
							Detected from the OS. Switch the preview to check the other platform.
						</div>
					</div>

					<WritesNote stepId="shortcuts" />
				</div>

				{/* ── Col B: everything else ──────────────────────────── */}
				<div>
					<div
						className="overflow-hidden rounded-md border"
						style={{ borderColor: 'var(--border)', background: 'var(--bg-surface)' }}
						data-testid="shortcuts-everything-else"
					>
						<div
							className="border-b px-3 py-2 text-[11px] font-semibold uppercase tracking-[0.06em]"
							style={{ borderColor: 'var(--border-soft)', color: 'var(--fg-muted)' }}
						>
							Everything else
						</div>
						<div className="p-3 text-xs">
							<Row name="Open the palette" value={key('palette.open')} />
							<Row name="Shortcut list" value={key('shortcuts.open')} />
							<Row
								name="Rebind any of them"
								value="Settings › Keys"
								title="The keybinding editor ships with the Phase 6 Actions & keys section."
							/>
							<p className="mt-3 text-[11px]" style={{ color: 'var(--fg-faint)' }}>
								The palette also lists the keys, grouped, and filters across every group as you
								type.
							</p>
							<Button
								variant="outline"
								size="sm"
								className="mt-3 h-8 gap-1.5 text-xs"
								onClick={() => setPaletteOpen(true)}
								data-testid="shortcuts-show-palette"
							>
								<Command className="h-3.5 w-3.5" />
								Show the palette
							</Button>
						</div>
					</div>
				</div>
			</div>

			<div className="mt-8 flex items-center justify-end gap-3">
				<Button onClick={onContinue} data-testid="shortcuts-inline-continue">
					Continue
				</Button>
			</div>

			{/* The wizard renders outside <Workspace>, which is where the shell's
			    own palette lives, so the step mounts one of its own. */}
			<CommandPalette open={paletteOpen} mode="all" onOpenChange={setPaletteOpen} />
		</div>
	);
}

function Row({ name, value, title }: { name: string; value: string; title?: string }) {
	return (
		<div
			className="flex items-center justify-between border-b py-1.5 last:border-b-0"
			style={{ borderColor: 'var(--border-soft)' }}
		>
			<span style={{ color: 'var(--fg-muted)' }}>{name}</span>
			<span className="font-mono" style={{ color: 'var(--fg)' }} title={title}>
				{value}
			</span>
		</div>
	);
}
