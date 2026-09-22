// WP-09 — the grouped Shortcuts view (§2 `?`, §6A.5, v4 P11).
//
// One overlay, one registry: this renders inside the ⌘K palette panel (mode
// `shortcuts`), opened by `?`, `⌘/`, the status-bar shortcuts item, or the
// palette's own "Keyboard shortcuts" row. Every row is generated from the
// keymap registry (`getKeymap()`) and every key hint goes through
// `labelFor()` — nothing here hard-codes a glyph, so the view cannot drift
// from the bindings that actually fire.
//
// Rows are reference-only text, never inert buttons (spec §1.2 / P11): the
// view is a filter input plus grouped definition lists. Typing filters across
// every group at once.

import { useEffect, useId, useMemo, useRef, useState } from 'react';
import { findEntry, getKeymap, type KeymapEntry, labelFor } from '@/lib/keymap/registry';
import { isMacPlatform } from '@/lib/keymap/platform';

/** Region a command belongs to, keyed by its namespace (the part before the
 *  first `.`). Order here is the order the groups render in — frame regions
 *  first, then the surfaces that own their own listeners. */
export const SHORTCUT_REGIONS: ReadonlyArray<{ id: string; label: string; namespaces: string[] }> =
	[
		{ id: 'rail', label: 'Rail', namespaces: ['rail'] },
		{ id: 'palette', label: 'Command palette', namespaces: ['palette'] },
		{ id: 'explorer', label: 'Explorer', namespaces: ['explorer'] },
		{ id: 'panes', label: 'Panes and tabs', namespaces: ['pane', 'tab'] },
		{ id: 'companion', label: 'Companion', namespaces: ['dock', 'companion', 'session'] },
		{ id: 'terminal', label: 'Terminal', namespaces: ['terminal'] },
		{ id: 'ngwa', label: 'Ngwa', namespaces: ['ngwa'] },
		{ id: 'menu', label: 'Menu bar', namespaces: ['menu'] },
		{ id: 'help', label: 'Help', namespaces: ['shortcuts'] },
	];

const OTHER_REGION = { id: 'other', label: 'Other' };

export function regionFor(command: string): { id: string; label: string } {
	const ns = command.split('.')[0] ?? '';
	return SHORTCUT_REGIONS.find((r) => r.namespaces.includes(ns)) ?? OTHER_REGION;
}

export interface ShortcutRow {
	command: string;
	label: string;
	keyLabel: string;
	/** Set when the binding only applies on the other platform. */
	platformNote: string | null;
}

export interface ShortcutGroup {
	id: string;
	label: string;
	rows: ShortcutRow[];
}

/**
 * Group the registry by region, one row per command id. A command with two
 * platform-gated entries (`terminal.clear`) collapses to the entry that
 * applies here (`findEntry`), so the view shows what this machine fires.
 */
export function groupShortcuts(
	entries: KeymapEntry[] = getKeymap(),
	opts?: { mac?: boolean }
): ShortcutGroup[] {
	const mac = opts?.mac ?? isMacPlatform();
	const seen = new Set<string>();
	const byRegion = new Map<string, ShortcutGroup>();
	for (const e of entries) {
		if (seen.has(e.command)) continue;
		seen.add(e.command);
		const entry = findEntry(e.command, { mac }) ?? e;
		const region = regionFor(e.command);
		let group = byRegion.get(region.id);
		if (!group) {
			group = { id: region.id, label: region.label, rows: [] };
			byRegion.set(region.id, group);
		}
		const applies = !entry.platformOnly || entry.platformOnly === (mac ? 'mac' : 'other');
		group.rows.push({
			command: e.command,
			label: entry.label,
			keyLabel: labelFor(e.command, { mac }),
			platformNote: applies ? null : entry.platformOnly === 'mac' ? 'macOS only' : 'not on macOS',
		});
	}
	const order = [...SHORTCUT_REGIONS.map((r) => r.id), OTHER_REGION.id];
	return Array.from(byRegion.values()).sort((a, b) => order.indexOf(a.id) - order.indexOf(b.id));
}

function matches(row: ShortcutRow, group: ShortcutGroup, q: string): boolean {
	if (!q) return true;
	const hay = `${row.label} ${row.command} ${row.keyLabel} ${group.label}`.toLowerCase();
	return q
		.toLowerCase()
		.split(/\s+/)
		.filter(Boolean)
		.every((term) => hay.includes(term));
}

export function ShortcutsView({ onBack }: { onBack?: () => void }) {
	const [query, setQuery] = useState('');
	const groups = useMemo(() => groupShortcuts(), []);
	const inputId = useId();
	const inputRef = useRef<HTMLInputElement | null>(null);
	// Focus the filter on open, like the palette's own search input.
	useEffect(() => {
		inputRef.current?.focus();
	}, []);
	const visible = groups
		.map((g) => ({ ...g, rows: g.rows.filter((r) => matches(r, g, query.trim())) }))
		.filter((g) => g.rows.length > 0);

	return (
		<div className="flex flex-col" data-testid="shortcuts-view">
			<div className="flex items-center gap-2 border-b border-border px-4">
				<label htmlFor={inputId} className="sr-only">
					Filter keyboard shortcuts
				</label>
				<input
					id={inputId}
					ref={inputRef}
					value={query}
					onChange={(e) => setQuery(e.target.value)}
					placeholder="Filter keyboard shortcuts…"
					className="min-w-0 flex-1 bg-transparent py-3 text-sm outline-none placeholder:text-muted-foreground"
				/>
				{onBack && (
					<button
						type="button"
						onClick={onBack}
						className="shrink-0 rounded-[var(--radius-xs)] px-2 py-1 text-xs text-muted-foreground outline-none hover:bg-accent hover:text-accent-foreground focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset"
					>
						All commands
					</button>
				)}
			</div>
			{/* Scrollable, keyboard-focusable region so the list can be read by
			    keyboard alone (arrow / page keys scroll it once focused). */}
			<section
				aria-label="Keyboard shortcuts"
				// biome-ignore lint/a11y/noNoninteractiveTabindex: a scrollable region must be focusable so keyboard users can scroll it (WCAG 2.1.1).
				tabIndex={0}
				className="max-h-[50vh] overflow-y-auto p-2 outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset"
			>
				{visible.length === 0 && (
					<div role="status" className="py-8 text-center text-sm text-muted-foreground">
						No shortcuts match.
					</div>
				)}
				{visible.map((g) => (
					<section key={g.id} aria-labelledby={`${inputId}-${g.id}`} data-region={g.id}>
						<h3
							id={`${inputId}-${g.id}`}
							className="px-2 pb-1 pt-2 text-xs font-medium text-muted-foreground"
						>
							{g.label}
						</h3>
						<dl className="flex flex-col">
							{g.rows.map((r) => (
								<div
									key={r.command}
									data-command={r.command}
									className="flex items-center gap-3 rounded-[var(--radius-sm)] px-2 py-1.5 text-sm"
								>
									<dt className="min-w-0 flex-1 truncate">
										{r.label}
										{r.platformNote && (
											<span className="ml-2 text-xs text-muted-foreground">{r.platformNote}</span>
										)}
									</dt>
									<dd>
										<kbd className="rounded-[var(--radius-xs)] border border-border bg-muted px-1.5 py-0.5 font-mono text-xs text-muted-foreground">
											{r.keyLabel}
										</kbd>
									</dd>
								</div>
							))}
						</dl>
					</section>
				))}
			</section>
		</div>
	);
}
