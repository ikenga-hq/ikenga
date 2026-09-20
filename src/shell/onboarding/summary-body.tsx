// Step 8 — Summary / finish.
//
// Reads each step's `payload` from the store, renders one card per
// step with an Edit link that re-enters the wizard in edit mode, and
// stamps `completedAt` on Open-workspace. The boot redirect in
// `__root.tsx` keys off `completedAt === null`, so once we stamp it
// the redirect stops firing.
//
// Refuses to finish if any step is still `pending` while the preflight
// has a known fail — per the doc, we surface that hint rather than
// silently completing.

import { useMemo, useState } from 'react';
import { useNavigate } from '@tanstack/react-router';

import { LoreTerm } from '@/components/lore/lore-term';
import { Button } from '@/components/ui/button';
import { dailyAddress } from '@/lib/lore';
import {
	ONBOARDING_STEPS,
	type OnboardingStepId,
	type OnboardingStepRecord,
	useShellStore,
} from '@/lib/shell/shell-store';
import { useIkengaStore } from '@/lib/ikenga/theme-store';

import type { AgentStepPayload } from './agent-body';
import type { AppearancePayload } from './appearance-body';
import type { RootsStepPayload } from './roots-body';
import type { ScaffoldingPayload } from './scaffolding-body';

interface SummaryBodyProps {
	/** From the wizard chrome. On the summary step `goNext` is wired to
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

export function SummaryBody({ onFinish, goTo }: SummaryBodyProps) {
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

	const blocker = findBlockingState(steps);

	// 700ms time-of-day greeting flourish before the route transition.
	// Per design/shell/concepts/.../PHASE-1B-LORE-DELTA.md §9. Re-rendered
	// each click so the greeting always reflects local time at finish.
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
						<div className="text-4xl font-bold tracking-tight" style={{ color: 'var(--primary)' }}>
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
					<h1 className="text-4xl font-bold leading-tight tracking-tight">
						Your <LoreTerm term="Ikenga">Ikenga</LoreTerm> is ready to be addressed.
					</h1>
					<p className="mt-3 max-w-[60ch] text-sm" style={{ color: 'var(--fg-muted)' }}>
						Here's everything you picked. Each row is reversible from{' '}
						<span className="font-mono text-xs">Settings → · · ·</span> — nothing here is locked in.
					</p>
				</div>
				<div
					className="flex flex-none items-center gap-3 rounded-md border px-4 py-3"
					style={{
						borderColor: 'var(--success)',
						background: 'var(--success-soft, var(--bg-surface))',
					}}
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

			<div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3" data-testid="summary-grid">
				{cards.map((card) => (
					<SummaryCard key={card.id} card={card} onEdit={() => goTo(card.id)} />
				))}
			</div>

			{blocker && (
				<div
					className="mt-6 rounded-md border p-4 text-sm"
					style={{
						borderColor: 'var(--danger)',
						background: 'var(--danger-soft)',
					}}
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
	welcome: 'Consecration',
	agent: 'Chi',
	roots: 'Obi',
	packages: 'Alusi',
	connectors: 'Connectors',
	scaffolding: 'Scaffolding',
	appearance: 'Appearance',
	summary: 'Summary',
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
		if (id === 'summary') continue; // no self-card
		const rec = steps[id];
		const card = renderCard(id, rec, ctx);
		cards.push(card);
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
		case 'agent': {
			const p = rec.payload as AgentStepPayload | undefined;
			if (!p)
				return {
					...base,
					value: rec.status === 'completed' ? 'Selected' : 'Not chosen',
				};
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
		case 'roots': {
			const p = rec.payload as RootsStepPayload | undefined;
			const rootCount = p?.extraRoots?.length ?? ctx.extraRoots.length;
			const rootSample =
				(p?.extraRoots ?? ctx.extraRoots).slice(0, 3).join('\n') || '(no project roots)';
			return {
				...base,
				value: `${rootCount} project root${rootCount === 1 ? '' : 's'}`,
				detail: rootSample,
			};
		}
		case 'packages': {
			// Phase 5 fills this in. For now, surface that the step is
			// pending — the summary should reflect reality.
			return {
				...base,
				value: rec.status === 'completed' ? 'Packages selected' : 'Pending (Phase 5)',
				detail:
					rec.status !== 'completed' ? 'Packages will land in a follow-up release.' : undefined,
			};
		}
		case 'connectors': {
			return {
				...base,
				value: rec.status === 'completed' ? 'Connectors configured' : 'Pending (Phase 5)',
				detail: rec.status !== 'completed' ? 'Connector wiring lands in Phase 5.' : undefined,
			};
		}
		case 'scaffolding': {
			const p = rec.payload as ScaffoldingPayload | undefined;
			if (!p)
				return {
					...base,
					value: rec.status === 'completed' ? 'Scaffolded' : 'Not chosen',
				};
			if (p.choice === 'na')
				return { ...base, value: 'N/A', detail: 'No starter pack for this agent yet.' };
			if (p.choice === 'skip')
				return { ...base, value: 'Skipped', detail: p.rootPath ?? undefined, skipped: true };
			if (p.choice === 'merge')
				return {
					...base,
					value: 'Merged into existing .claude/',
					detail: p.rootPath ?? undefined,
				};
			return {
				...base,
				value: `Starter pack (${p.profile})`,
				detail: p.rootPath ?? undefined,
			};
		}
		case 'appearance': {
			const p = rec.payload as AppearancePayload | undefined;
			const theme = p?.theme ?? (ctx.theme as AppearancePayload['theme']);
			const mode = p?.mode ?? (ctx.mode as AppearancePayload['mode']);
			const density = p?.density ?? (ctx.density as AppearancePayload['density']);
			return {
				...base,
				value: `${themeName(theme)} · ${capitalise(mode)} · ${capitalise(density)}`,
				detail: `Theme ${theme}`,
			};
		}
		default:
			return base;
	}
}

function themeName(t: AppearancePayload['theme']): string {
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

/** Public for tests. Returns null when nothing blocks "Enter your Obi". */
export function findBlockingState(
	steps: Record<OnboardingStepId, OnboardingStepRecord>
): string | null {
	// Required step gate: Chi must be chosen (offline mode counts as
	// completed). Welcome must be completed (preflight passed).
	if (steps.welcome.status !== 'completed') {
		return 'Step 1 (Consecration) is incomplete — go back and review the system checks.';
	}
	if (steps.agent.status === 'pending') {
		return 'Step 2 (Chi) is still pending — pick a Chi or continue offline.';
	}
	return null;
}
