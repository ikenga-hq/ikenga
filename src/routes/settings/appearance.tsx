import { createFileRoute } from '@tanstack/react-router';
import { confirm as confirmDialog } from '@/lib/transport/dialog-shim';
import { Monitor, Moon, RotateCcw, Sun } from 'lucide-react';
import { useCallback, useRef, useState } from 'react';

import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { Switch } from '@/components/ui/switch';
import { cn } from '@/components/ui/utils';
import {
	type IkengaDensity,
	type IkengaMode,
	type IkengaTheme,
	type IkengaTintStrength,
	useIkengaStore,
} from '@/lib/ikenga/theme-store';
import { writeSettingsField } from '@/lib/settings/client';
import type { SettingsWriteOptions } from '@/lib/settings/types';
import { loadAppDb } from '@/lib/sql-db';
import { SettingsFieldRow, useSettingsSection } from '@/shell/settings/field';

import { LAYOUT_LS_PREFIX } from './-components/clear-data';

interface AppearanceProjectValueMap {
	'appearance.theme': IkengaTheme;
	'appearance.mode': IkengaMode;
	'appearance.density': IkengaDensity;
	'appearance.tintStrength': IkengaTintStrength;
}

// Per-theme preview palettes — driven by the theme value, not the host theme,
// so each card always renders in its own colors.
interface ThemePreviewPalette {
	bg: string;
	surface: string;
	text: string;
	accent: string;
	dot: string;
}

const THEMES: Array<{
	value: IkengaTheme;
	label: string;
	preview: ThemePreviewPalette;
}> = [
	{
		value: 'A',
		label: 'Dusk Wood',
		preview: {
			bg: 'hsl(28, 18%, 4%)',
			surface: 'hsl(28, 14%, 14%)',
			text: 'hsl(36, 28%, 90%)',
			accent: 'hsl(20, 50%, 34%)',
			dot: 'hsl(20, 50%, 34%)',
		},
	},
	{
		value: 'B',
		label: 'Kola Daylight',
		preview: {
			bg: 'hsl(36, 12%, 8%)',
			surface: 'hsl(36, 10%, 18%)',
			text: 'hsl(40, 28%, 92%)',
			accent: 'hsl(42, 84%, 60%)',
			dot: 'hsl(42, 84%, 60%)',
		},
	},
	{
		value: 'C',
		label: 'Bronze Shrine',
		preview: {
			bg: 'hsl(180, 14%, 7%)',
			surface: 'hsl(180, 12%, 16%)',
			text: 'hsl(40, 18%, 90%)',
			accent: 'hsl(170, 35%, 50%)',
			dot: 'hsl(170, 35%, 50%)',
		},
	},
];

const DENSITIES: Array<{ value: IkengaDensity; label: string; gap: string }> = [
	{ value: 'compact', label: 'Compact', gap: '1px' },
	{ value: 'comfortable', label: 'Comfortable', gap: '3px' },
	{ value: 'spacious', label: 'Spacious', gap: '5px' },
];

const TINTS: Array<{ value: IkengaTintStrength; label: string }> = [
	{ value: 'off', label: 'Off' },
	{ value: 'subtle', label: 'Subtle' },
	{ value: 'strong', label: 'Strong' },
];

function AppearancePage() {
	const { scope, projectId, result, refresh } = useSettingsSection();
	const theme = useIkengaStore((s) => s.theme);
	const mode = useIkengaStore((s) => s.mode);
	const density = useIkengaStore((s) => s.density);
	const tintStrength = useIkengaStore((s) => s.tintStrength);
	const setTheme = useIkengaStore((s) => s.setTheme);
	const setMode = useIkengaStore((s) => s.setMode);
	const setDensity = useIkengaStore((s) => s.setDensity);
	const setTintStrength = useIkengaStore((s) => s.setTintStrength);

	const isProject = scope === 'project';
	const appearance = result?.effective.appearance ?? {};

	async function writeProject<K extends keyof AppearanceProjectValueMap>(
		field: K,
		value: AppearanceProjectValueMap[K]
	) {
		if (!projectId) return;
		await writeSettingsField({ scope: 'project', field, value, projectId } as SettingsWriteOptions);
		refresh();
	}

	return (
		<div className="mx-auto w-full max-w-[720px] space-y-6 px-6 py-6">
			<header className="space-y-1">
				<h2
					className="text-2xl font-semibold tracking-tight"
					style={{ fontFamily: 'var(--font-display)' }}
				>
					Appearance
				</h2>
				<p className="max-w-prose text-sm leading-relaxed text-muted-foreground">
					Theme, mode and density are the three knobs that scope every other token. Changes apply
					instantly across all panes and pkg iframes.
				</p>
			</header>

			<section className="overflow-hidden rounded-lg border border-[var(--border-soft)] bg-card">
				<header className="border-b border-[var(--border-soft)] bg-[var(--bg-sunken)] px-4 py-2.5">
					<h3 className="text-xs font-semibold uppercase tracking-wider text-muted-foreground">
						Theme
					</h3>
				</header>
				<div className="divide-y divide-border">
					<SettingsFieldRow
						field="appearance.theme"
						label="Theme"
						desc="Dusk Wood is canonical. Kola Daylight (B) and Bronze Shrine (C) are alternates — every pkg is required to render correctly under all three."
						stacked
					>
						<div className="grid grid-cols-1 gap-3 sm:grid-cols-3">
							{THEMES.map((opt) => (
								<ThemeCard
									key={opt.value}
									label={opt.label}
									palette={opt.preview}
									active={(isProject ? appearance.theme ?? theme : theme) === opt.value}
									onSelect={() => {
										if (isProject) void writeProject('appearance.theme', opt.value);
										else setTheme(opt.value);
									}}
								/>
							))}
						</div>
					</SettingsFieldRow>

					<SettingsFieldRow
						field="appearance.mode"
						label="Mode"
						desc="Light or dark. Modulates lightness only; hues stay constant per theme."
					>
						<SegmentedControl<IkengaMode>
							value={(isProject ? (appearance.mode as IkengaMode | undefined) ?? mode : mode)}
							onChange={(next) => {
								if (isProject) void writeProject('appearance.mode', next);
								else setMode(next);
							}}
							ariaLabel="Mode"
							items={[
								{ value: 'light', label: 'Light', Icon: Sun },
								{ value: 'dark', label: 'Dark', Icon: Moon },
								{
									value: 'system',
									label: 'System',
									Icon: Monitor,
									title: 'Follow OS preference',
								},
							]}
						/>
					</SettingsFieldRow>

					<SettingsFieldRow
						field="appearance.density"
						label="Density"
						desc="Row heights, type sizes and pane padding scale together. Compact is good for big monitors, spacious for laptops."
					>
						<SegmentedControl<IkengaDensity>
							value={
								(isProject ? (appearance.density as IkengaDensity | undefined) ?? density : density)
							}
							onChange={(next) => {
								if (isProject) void writeProject('appearance.density', next);
								else setDensity(next);
							}}
							ariaLabel="Density"
							items={DENSITIES.map((d) => ({
								value: d.value,
								label: d.label,
								glyph: <DensityGlyph gap={d.gap} />,
							}))}
						/>
					</SettingsFieldRow>
				</div>
			</section>

			<section className="overflow-hidden rounded-lg border border-[var(--border-soft)] bg-card">
				<header className="border-b border-[var(--border-soft)] bg-[var(--bg-sunken)] px-4 py-2.5">
					<h3 className="text-xs font-semibold uppercase tracking-wider text-muted-foreground">
						Type
					</h3>
				</header>
				<div className="divide-y divide-border">
					<SettingStubRow
						label="Reading width"
						desc="Caps the email reader, settings panes and any prose surface. Default 64ch."
						stubId="reading-width"
					>
						<Input
							type="text"
							defaultValue="64ch"
							disabled
							aria-describedby="stub-desc-reading-width"
							className="h-8 w-28 font-mono text-xs"
						/>
					</SettingStubRow>

					<SettingStubRow
						label="Reduce motion"
						desc="Drop pane transitions, sheet animations, and the dock collapse curl. Honours prefers-reduced-motion by default."
						stubId="reduce-motion"
					>
						<Switch disabled aria-describedby="stub-desc-reduce-motion" />
					</SettingStubRow>
				</div>
			</section>

			<section className="overflow-hidden rounded-lg border border-[var(--border-soft)] bg-card">
				<header className="border-b border-[var(--border-soft)] bg-[var(--bg-sunken)] px-4 py-2.5">
					<h3 className="text-xs font-semibold uppercase tracking-wider text-muted-foreground">
						Workspace tint
					</h3>
				</header>
				<div className="divide-y divide-border">
					<SettingsFieldRow
						field="appearance.tintStrength"
						label="Tint strength"
						desc="Controls how much each workspace recolors the sidebar head and active accents."
					>
						<SegmentedControl<IkengaTintStrength>
							value={
								(isProject
									? (appearance.tintStrength as IkengaTintStrength | undefined) ?? tintStrength
									: tintStrength)
							}
							onChange={(next) => {
								if (isProject) void writeProject('appearance.tintStrength', next);
								else setTintStrength(next);
							}}
							ariaLabel="Tint strength"
							items={TINTS.map((t) => ({ value: t.value, label: t.label }))}
						/>
					</SettingsFieldRow>
					<div className="px-4 py-2 text-[11px] italic text-muted-foreground">
						Per-workspace overrides are coming.
					</div>
				</div>
			</section>

			<section className="overflow-hidden rounded-lg border border-[var(--border-soft)] bg-card">
				<header className="border-b border-[var(--border-soft)] bg-[var(--bg-sunken)] px-4 py-2.5">
					<h3 className="text-xs font-semibold uppercase tracking-wider text-muted-foreground">
						Layout
					</h3>
				</header>
				<div className="divide-y divide-border">
					<SettingStubRow
						label="Activity bar width"
						desc="The left rail with workspace icons. Default 56px."
						stubId="activity-bar-width"
					>
						<Input
							type="text"
							defaultValue="56px"
							disabled
							aria-describedby="stub-desc-activity-bar-width"
							className="h-8 w-28 font-mono text-xs"
						/>
					</SettingStubRow>

					<SettingStubRow
						label="Sidebar default"
						desc="Whether the sidebar starts open or collapsed when you switch workspaces."
						stubId="sidebar-default"
					>
						<select
							disabled
							aria-describedby="stub-desc-sidebar-default"
							className="h-8 rounded-md border border-border bg-background px-2 text-xs disabled:opacity-50"
						>
							<option>open</option>
							<option>collapsed</option>
							<option>remember per workspace</option>
						</select>
					</SettingStubRow>

					<SettingStubRow
						label="Dock pane default"
						desc="The bottom pane on a fresh window — terminal, viewer, or off."
						stubId="dock-pane-default"
					>
						<select
							disabled
							aria-describedby="stub-desc-dock-pane-default"
							className="h-8 rounded-md border border-border bg-background px-2 text-xs disabled:opacity-50"
						>
							<option>Terminal</option>
							<option>Viewer</option>
							<option>Off</option>
						</select>
					</SettingStubRow>

					<SettingStubRow
						label="Reset layout"
						desc="Restore default panel sizes and sidebar states across all workspaces."
						stubId="reset-layout"
					>
						<ResetLayoutButton />
					</SettingStubRow>
				</div>
			</section>
		</div>
	);
}

function SettingStubRow({
	label,
	desc,
	stubId,
	children,
}: {
	label: string;
	desc: string;
	stubId: string;
	children: React.ReactNode;
}) {
	return (
		<div className="grid grid-cols-[1fr_auto] items-center gap-4 px-4 py-3">
			<div className="min-w-0 space-y-0.5">
				<div className="text-sm font-medium text-foreground">{label}</div>
				<div id={`stub-desc-${stubId}`} className="text-xs leading-relaxed text-muted-foreground">
					{desc}
				</div>
			</div>
			<div className="shrink-0">{children}</div>
		</div>
	);
}

interface SegmentedItem<T extends string> {
	value: T;
	label: string;
	Icon?: React.ComponentType<{ className?: string }>;
	glyph?: React.ReactNode;
	disabled?: boolean;
	title?: string;
}

function SegmentedControl<T extends string>({
	value,
	onChange,
	items,
	ariaLabel,
}: {
	value: T;
	onChange: (v: T) => void;
	items: SegmentedItem<T>[];
	ariaLabel?: string;
}) {
	const groupRef = useRef<HTMLDivElement | null>(null);

	const enabled = items.filter((i) => !i.disabled);

	const focusItem = useCallback((val: string) => {
		requestAnimationFrame(() => {
			groupRef.current?.querySelector<HTMLElement>(`[data-seg-val="${val}"]`)?.focus();
		});
	}, []);

	const onKeyDown = useCallback(
		(e: React.KeyboardEvent<HTMLDivElement>) => {
			if (!['ArrowLeft', 'ArrowRight', 'Home', 'End'].includes(e.key)) return;
			if (enabled.length === 0) return;
			const pos = enabled.findIndex((i) => i.value === value);
			let next = pos;
			if (e.key === 'ArrowRight') next = pos < 0 ? 0 : Math.min(pos + 1, enabled.length - 1);
			else if (e.key === 'ArrowLeft') next = pos < 0 ? 0 : Math.max(pos - 1, 0);
			else if (e.key === 'Home') next = 0;
			else if (e.key === 'End') next = enabled.length - 1;
			const target = enabled[next];
			if (!target || target.value === value) {
				e.preventDefault();
				return;
			}
			e.preventDefault();
			onChange(target.value);
			focusItem(target.value);
		},
		[enabled, value, onChange, focusItem]
	);

	return (
		<div
			ref={groupRef}
			role="group"
			aria-label={ariaLabel}
			onKeyDown={onKeyDown}
			className="inline-flex items-center gap-0.5 rounded-md border border-border p-0.5"
			style={{ background: 'var(--bg-base)' }}
		>
			{items.map((item) => {
				const Icon = item.Icon;
				const active = value === item.value;
				return (
					<button
						key={item.value}
						type="button"
						disabled={item.disabled}
						title={item.title}
						aria-pressed={active}
						data-seg-val={item.value}
						onClick={() => !item.disabled && onChange(item.value)}
						className={cn(
							'inline-flex items-center gap-1.5 rounded px-2.5 py-1 text-xs transition-colors',
							'outline-none focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-primary',
							active
								? 'bg-card text-foreground shadow-sm'
								: 'text-muted-foreground hover:text-foreground',
							item.disabled && 'cursor-not-allowed opacity-50'
						)}
					>
						{Icon && <Icon className="h-3.5 w-3.5" />}
						{item.glyph}
						<span>{item.label}</span>
					</button>
				);
			})}
		</div>
	);
}

function DensityGlyph({ gap }: { gap: string }) {
	return (
		<span aria-hidden="true" className="inline-flex flex-col" style={{ gap, width: '12px' }}>
			<span className="block h-[1.5px] w-full rounded bg-current" />
			<span className="block h-[1.5px] w-full rounded bg-current" />
			<span className="block h-[1.5px] w-full rounded bg-current" />
		</span>
	);
}

function ThemeCard({
	label,
	palette,
	active,
	onSelect,
}: {
	label: string;
	palette: ThemePreviewPalette;
	active: boolean;
	onSelect: () => void;
}) {
	return (
		<button
			type="button"
			onClick={onSelect}
			aria-pressed={active}
			className={cn(
				'group flex cursor-pointer flex-col overflow-hidden rounded-md border bg-card text-left transition-colors',
				'outline-none focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-primary',
				active
					? 'border-primary shadow-[inset_0_0_0_1px_var(--primary)]'
					: 'border-border-soft hover:border-foreground/20'
			)}
		>
			<div
				aria-hidden="true"
				className="grid h-16 grid-rows-[auto_1fr_auto] gap-[5px] border-b border-border-soft p-2"
				style={{ background: palette.bg }}
			>
				<div className="flex items-center gap-1">
					<span
						className="h-[5px] w-[5px] shrink-0 rounded-full"
						style={{ background: palette.dot }}
					/>
					<span className="h-1 w-7 shrink-0 rounded-[2px]" style={{ background: palette.text }} />
				</div>
				<div className="flex items-center gap-1">
					<span
						className="h-1 w-[52px] shrink-0 rounded-[2px]"
						style={{ background: palette.surface }}
					/>
					<span className="h-1 flex-1 rounded-[2px]" style={{ background: palette.surface }} />
				</div>
				<div className="flex items-center gap-1">
					<span
						className="h-1 w-[18px] shrink-0 rounded-[2px]"
						style={{ background: palette.accent }}
					/>
					<span className="h-1 flex-1 rounded-[2px]" style={{ background: palette.surface }} />
				</div>
			</div>
			<div
				className="flex items-center justify-between gap-2 px-2.5 py-1.5"
				style={{
					background: 'var(--bg-sunken)',
					fontFamily: 'var(--font-mono)',
					fontSize: '10.5px',
					letterSpacing: '0.06em',
				}}
			>
				<span className="font-medium text-foreground">{label}</span>
				<span
					className={cn(
						'h-3 w-3 rounded-full border',
						active ? 'border-primary bg-primary' : 'border-border'
					)}
				/>
			</div>
		</button>
	);
}

function ResetLayoutButton() {
	const [busy, setBusy] = useState(false);

	async function handleReset() {
		const ok = await confirmDialog(
			'Reset all panel sizes, pane tree, terminal tabs, and dock state? The window will reload.',
			{ title: 'Reset workspace layout', kind: 'warning' }
		);
		if (!ok) return;
		setBusy(true);
		try {
			try {
				const db = await loadAppDb();
				await db.execute('DELETE FROM layout_state');
			} catch (e) {
				console.warn('[settings] failed to clear layout_state', e);
			}
			const toRemove: string[] = [];
			for (let i = 0; i < localStorage.length; i++) {
				const k = localStorage.key(i);
				if (!k) continue;
				if (k.startsWith(LAYOUT_LS_PREFIX)) toRemove.push(k);
			}
			for (const k of toRemove) localStorage.removeItem(k);
			localStorage.removeItem('ikenga-dock');
			localStorage.removeItem('terminal.tabs');
			window.location.reload();
		} finally {
			setBusy(false);
		}
	}

	return (
		<Button variant="outline" size="sm" onClick={handleReset} disabled={busy} className="text-red-700">
			<RotateCcw className="mr-1 h-3.5 w-3.5" />
			Reset
		</Button>
	);
}

export const Route = createFileRoute('/settings/appearance')({
	component: AppearancePage,
});
