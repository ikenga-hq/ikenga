// Step 7 (D-04 `done`) — what was set up. Renamed from the shipped
// `summary` step (`summary-body.tsx`, retired by this file) as part of
// WP-38's re-map. Reads every step's `payload` from the store, renders one
// card per step with an Edit link that re-enters the wizard in edit mode,
// and stamps `completedAt` on Open-workspace. The boot redirect in
// `__root.tsx` keys off `completedAt === null`, so once we stamp it the
// redirect stops firing.
//
// Adds the D-04 "Files written" panel (`designs/onboarding.html` `VIEW.done`
// `files` list) — the concrete settings.json / pkgs / `.claude/` paths this
// run touched, each with an "Open" action where one exists. This is the
// summary WP-39's daily address links back into, per `05-tracking.md`
// WP-38's "Produces" line.

import { useMemo, useState } from 'react';
import { useNavigate } from '@tanstack/react-router';

import { LoreTerm } from '@/components/lore/lore-term';
import { Button } from '@/components/ui/button';
import { dailyAddress } from '@/lib/lore';
import { openSettingsFile } from '@/lib/settings/client';
import {
	ONBOARDING_STEPS,
	type OnboardingStepId,
	type OnboardingStepRecord,
	useShellStore,
} from '@/lib/shell/shell-store';
import { useIkengaStore } from '@/lib/ikenga/theme-store';

import type { EngineStepPayload } from './engine-body';
import type { EquipmentStepPayload } from './equipment-body';
import type { LookPayload } from './look-body';
import type { ProjectStepPayload } from './project-body';

interface DoneBodyProps {
	/** From the wizard chrome. On the `done` step `goNext` is wired to
	 *  `finishOnboarding()` already; we still navigate manually so the
	 *  user lands on `/`. */
	onFinish: () => void;
	goTo: (id: OnboardingStepId) => void;
}

interface CardModel {
	id: OnboardingStepId;
	label: string;
	value: string;
	detail?: string;
	skipped?: boolean;
}

export function DoneBody({ onFinish, goTo }: DoneBodyProps) {
	const navigate = useNavigate();
	const steps = useShellStore((s) => s.onboarding.steps);
	const startedAt = useShellStore((s) => s.onboarding.startedAt);
	const userName = useShellStore((s) => s.userName);
	const activeProject = useShellStore((s) => s.activeProject);
	const extraRoots = activeProject?.extra_roots ?? [];
	const theme = useIkengaStore((s) => s.theme);
	const mode = useIkengaStore((s) => s.mode);
	const density = useIkengaStore((s) => s.density);

	const cards: CardModel[] = useMemo(
		() => buildCards(steps, { extraRoots, theme, mode, density }),
		[steps, extraRoots, theme, mode, density]
	);

	const filesWritten = useMemo(() => {
		const files = ['~/.ikenga/settings.json'];
		const equipmentPayload = steps.equipment.payload as EquipmentStepPayload | undefined;
		if ((equipmentPayload?.selected?.length ?? 0) > 0) files.push('~/.ikenga/pkgs/');
		if (
			equipmentPayload?.scaffolding &&
			(equipmentPayload.scaffolding.choice === 'scaffold' ||
				equipmentPayload.scaffolding.choice === 'merge')
		) {
			const root = equipmentPayload.scaffolding.rootPath;
			if (root) files.push(`${root}/.claude/`);
		}
		if (activeProject?.root_path && projectHasSettingsFile(activeProject.id)) {
			files.push(`${activeProject.root_path}/.ikenga/settings.json`);
		}
		return files;
	}, [steps.equipment.payload, activeProject]);

	const blocker = findBlockingState(steps);

	// 700ms time-of-day greeting flourish before the route transition.
	const [greeting, setGreeting] = useState<{ igbo: string; english: string } | null>(null);

	const handleOpenWorkspace = () => {
		if (blocker) return;
		const g = dailyAddress(new Date());
		setGreeting({ igbo: g.igbo, english: g.english });
		window.setTimeout(() => {
			onFinish();
			void navigate({ to: '/' });
		}, 700);
	};

	return (
		<div className="relative mx-auto max-w-5xl">
			{greeting && (
				<div
					className="pointer-events-none absolute inset-0 z-10 flex animate-in items-center justify-center fade-in-0 duration-200"
					style={{ background: 'var(--bg-base)' }}
					data-testid="summary-greeting-flourish"
					aria-live="polite"
				>
					<div className="text-center">
						<div className="font-display text-4xl font-bold tracking-tight" style={{ color: 'var(--primary)' }}>
							{greeting.igbo}
							{userName ? `, ${userName}` : ''}.
						</div>
						<div className="mt-2 text-base" style={{ color: 'var(--fg-muted)' }}>
							{greeting.english}
							{userName ? `, ${userName}` : ''}.
						</div>
					</div>
				</div>
			)}
			<div className="mb-8 flex items-start justify-between gap-6">
				<div>
					<p
						className="mb-2 text-xs font-semibold uppercase tracking-[0.04em]"
						style={{ color: 'var(--primary)' }}
					>
						<LoreTerm term="Consecration">Consecration</LoreTerm> complete
					</p>
					<h1 className="font-display text-4xl font-bold leading-tight tracking-tight">
						Your <LoreTerm term="Ikenga">Ikenga</LoreTerm> is consecrated.
					</h1>
					<p className="mt-3 max-w-[60ch] text-sm" style={{ color: 'var(--fg-muted)' }}>
						Here's everything you picked. Each row is reversible from Settings — nothing here is
						locked in.
					</p>
				</div>
				<div
					className="flex flex-none items-center gap-3 rounded-md border px-4 py-3"
					style={{ borderColor: 'var(--success)', background: 'var(--success-soft, var(--bg-surface))' }}
					data-testid="summary-ready-mark"
				>
					<div
						className="flex h-7 w-7 flex-none items-center justify-center rounded-full text-xs font-bold"
						style={{ background: 'var(--success)', color: 'var(--success-fg, white)' }}
						aria-hidden="true"
					>
						✓
					</div>
					<div>
						<div className="text-[13px] font-semibold">Setup complete</div>
						<div className="text-[11.5px]" style={{ color: 'var(--fg-muted)' }}>
							{startedAt ? `Started ${formatRelative(startedAt)}` : 'Reviewed your choices'}
						</div>
					</div>
				</div>
			</div>

			<div className="grid gap-6 lg:grid-cols-[1.3fr_1fr]">
				<div className="grid gap-4 sm:grid-cols-2" data-testid="summary-grid">
					{cards.map((card) => (
						<SummaryCard key={card.id} card={card} onEdit={() => goTo(card.id)} />
					))}
				</div>

				<div
					className="rounded-lg border p-4"
					style={{ borderColor: 'var(--border-soft)', background: 'var(--bg-surface)' }}
					data-testid="done-files-written"
				>
					<p
						className="mb-2 text-[11px] font-semibold uppercase tracking-[0.05em]"
						style={{ color: 'var(--fg-faint)' }}
					>
						Files written · {filesWritten.length}
					</p>
					<div className="grid gap-1.5">
						{filesWritten.map((f) => (
							<div key={f} className="flex items-center justify-between gap-3 text-xs">
								<span className="truncate font-mono" style={{ color: 'var(--fg)' }} title={f}>
									{f}
								</span>
								{f === '~/.ikenga/settings.json' && (
									<Button
										variant="ghost"
										size="sm"
										className="h-6 px-2 text-[11px]"
										onClick={() => void openSettingsFile('personal').catch(() => {})}
									>
										Open
									</Button>
								)}
							</div>
						))}
					</div>
					<p className="mt-3 text-[11px]" style={{ color: 'var(--fg-faint)' }}>
						No secret was written to any of these. Keys live in the vault.
					</p>
				</div>
			</div>

			{blocker && (
				<div
					className="mt-6 rounded-md border p-4 text-sm"
					style={{ borderColor: 'var(--danger)', background: 'var(--danger-soft)' }}
					data-testid="summary-blocker"
				>
					{blocker}
				</div>
			)}

			<div className="mt-8 flex items-center justify-end gap-3">
				<Button
					onClick={handleOpenWorkspace}
					disabled={!!blocker || !!greeting}
					data-testid="summary-open-workspace"
					className="h-11 px-6 text-sm font-semibold"
				>
					Enter your Obi (open workspace)
				</Button>
			</div>
		</div>
	);
}

function SummaryCard({ card, onEdit }: { card: CardModel; onEdit: () => void }) {
	return (
		<div
			className="rounded-lg border p-4"
			style={{
				borderColor: 'var(--border-soft)',
				background: 'var(--bg-surface)',
				opacity: card.skipped ? 0.7 : 1,
			}}
			data-testid="summary-card"
			data-step-id={card.id}
			data-skipped={!!card.skipped}
		>
			<div className="flex items-center justify-between">
				<span
					className="text-[11px] font-semibold uppercase tracking-[0.05em]"
					style={{ color: 'var(--fg-faint)' }}
				>
					{card.label}
				</span>
				<button
					type="button"
					onClick={onEdit}
					className="text-[11px] underline-offset-2 hover:underline"
					style={{ color: 'var(--primary)' }}
					data-testid="summary-edit"
				>
					Edit
				</button>
			</div>
			<div className="mt-2 text-[14px] font-semibold">{card.value}</div>
			{card.detail && (
				<div
					className="mt-1 line-clamp-4 whitespace-pre-line text-[12px]"
					style={{ color: 'var(--fg-muted)' }}
				>
					{card.detail}
				</div>
			)}
		</div>
	);
}

// ── Pure card builders / formatting ─────────────────────────────────────

const STEP_LABEL: Record<OnboardingStepId, string> = {
	welcome: 'Welcome',
	engine: 'Chi',
	project: 'Project',
	equipment: 'Ngwa',
	look: 'Look',
	shortcuts: 'Keys',
	done: 'Done',
};

interface ContextSnapshot {
	extraRoots: string[];
	theme: string;
	mode: string;
	density: string;
}

export function buildCards(
	steps: Record<OnboardingStepId, OnboardingStepRecord>,
	ctx: ContextSnapshot
): CardModel[] {
	const cards: CardModel[] = [];
	for (const id of ONBOARDING_STEPS) {
		if (id === 'done') continue; // no self-card
		const rec = steps[id];
		cards.push(renderCard(id, rec, ctx));
	}
	return cards;
}

function renderCard(
	id: OnboardingStepId,
	rec: OnboardingStepRecord,
	ctx: ContextSnapshot
): CardModel {
	const base: CardModel = { id, label: STEP_LABEL[id], value: '—' };
	if (rec.status === 'skipped') {
		return { ...base, value: 'Skipped', skipped: true };
	}
	switch (id) {
		case 'welcome': {
			return {
				...base,
				value: rec.status === 'completed' ? 'Preflight passed' : 'Not reviewed',
				detail: rec.status === 'completed' ? 'System checks looked OK.' : undefined,
			};
		}
		case 'engine': {
			const p = rec.payload as EngineStepPayload | undefined;
			if (!p) return { ...base, value: rec.status === 'completed' ? 'Selected' : 'Not chosen' };
			return {
				...base,
				value: p.display ?? p.agentId,
				detail: p.executablePath
					? `${p.executablePath}${p.version ? `\nv${p.version}` : ''}`
					: p.agentId === 'engine-noop'
						? 'Offline mode — AI features dormant.'
						: undefined,
			};
		}
		case 'project': {
			const p = rec.payload as ProjectStepPayload | undefined;
			const rootCount = p?.extraRoots?.length ?? ctx.extraRoots.length;
			const rootSample = (p?.extraRoots ?? ctx.extraRoots).slice(0, 3).join('\n') || '(no project roots)';
			return {
				...base,
				value: `${rootCount} project root${rootCount === 1 ? '' : 's'}`,
				detail: rootSample,
			};
		}
		case 'equipment': {
			const p = rec.payload as EquipmentStepPayload | undefined;
			if (!p || rec.status !== 'completed') {
				return { ...base, value: 'Not gathered' };
			}
			const pkgCount = p.selected.length;
			const scaffold = p.scaffolding;
			const scaffoldNoteFor: Record<string, string> = {
				scaffold: ' · .claude/ scaffolded',
				merge: ' · .claude/ merged',
				adopt: ' · .claude/ adopted as-is',
				leave: ' · .claude/ left alone',
			};
			const scaffoldNote = scaffold ? (scaffoldNoteFor[scaffold.choice] ?? '') : '';
			return {
				...base,
				value: `${pkgCount} package${pkgCount === 1 ? '' : 's'}${scaffoldNote}`,
				detail: p.selected.slice(0, 6).join(', ') || undefined,
			};
		}
		case 'look': {
			const p = rec.payload as LookPayload | undefined;
			const theme = p?.theme ?? (ctx.theme as LookPayload['theme']);
			const mode = p?.mode ?? (ctx.mode as LookPayload['mode']);
			const density = p?.density ?? (ctx.density as LookPayload['density']);
			return {
				...base,
				value: `${themeName(theme)} · ${capitalise(mode)} · ${capitalise(density)}`,
				detail: `Theme ${theme}`,
			};
		}
		case 'shortcuts': {
			return { ...base, value: rec.status === 'completed' ? 'Reviewed' : 'Not reviewed' };
		}
		default:
			return base;
	}
}

function themeName(t: LookPayload['theme']): string {
	switch (t) {
		case 'A':
			return 'Dusk Wood';
		case 'B':
			return 'Kola Daylight';
		case 'C':
			return 'Bronze Shrine';
		default:
			return `Theme ${t}`;
	}
}

function capitalise(s: string): string {
	return s.length > 0 ? s[0]!.toUpperCase() + s.slice(1) : s;
}

function formatRelative(ms: number): string {
	const diff = Date.now() - ms;
	const minutes = Math.max(1, Math.round(diff / 60_000));
	if (minutes < 60) return `${minutes} min ago`;
	const hours = Math.round(minutes / 60);
	if (hours < 24) return `${hours}h ago`;
	const days = Math.round(hours / 24);
	return `${days}d ago`;
}

/** Best-effort — there's no per-project "does settings.json exist" query
 *  exposed to onboarding; this only gates on the project having a root at
 *  all (a project-scoped write only ever happens then, per
 *  `rootSettingsEntry` in `shell-store.ts`). */
function projectHasSettingsFile(projectId: string): boolean {
	return projectId !== 'default';
}

/** Public for tests. Returns null when nothing blocks "Enter your Obi". */
export function findBlockingState(
	steps: Record<OnboardingStepId, OnboardingStepRecord>
): string | null {
	if (steps.welcome.status !== 'completed') {
		return 'Step 1 (Welcome) is incomplete — go back and review the system checks.';
	}
	if (steps.engine.status === 'pending') {
		return 'Step 2 (Chi) is still pending — pick a Chi or continue offline.';
	}
	return null;
}
