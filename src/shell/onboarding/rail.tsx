// D-04 step rail — `designs/onboarding.html` left column (`#stepList`).
//
// A step is reachable (clickable) once the wizard has visited it or moved
// past it — mirrors the mock's `S.visited[st.id] || done` gate. The store
// doesn't track a separate "visited" flag (WP-38 didn't want to widen
// `OnboardingStepRecord` for this alone), so it's derived: a step is
// reachable once its own status is no longer 'pending', or its index is at
// or before the current `activeIndex` (the user is allowed to step back
// through everything they've already walked past).

import { Link } from '@tanstack/react-router';

import {
	ONBOARDING_STEPS,
	type OnboardingStepId,
	type OnboardingStepRecord,
} from '@/lib/shell/shell-store';
import { cn } from '@/components/ui/utils';

// Rail copy — step name + one-line sub, per `designs/onboarding.html` `STEPS`.
// Kept separate from the wizard-stepper header labels (`STEP_LABELS`) so the
// rail can carry a longer descriptive sub-line without cramping the header.
export const RAIL_COPY: Record<OnboardingStepId, { nm: string; sub: string }> = {
	welcome: { nm: 'Welcome', sub: 'What this is' },
	engine: { nm: 'Chi', sub: 'Your engine' },
	project: { nm: 'Project', sub: 'The container' },
	equipment: { nm: 'Ngwa', sub: 'Your equipment' },
	look: { nm: 'Look', sub: 'Theme · mode · density' },
	shortcuts: { nm: 'Keys', sub: 'Five that matter' },
	done: { nm: 'Done', sub: 'What was set up' },
};

interface OnboardingRailProps {
	activeStepId: OnboardingStepId;
	activeIndex: number;
	steps: Record<OnboardingStepId, OnboardingStepRecord>;
	onNavigate: (id: OnboardingStepId) => void;
}

export function OnboardingRail({ activeStepId, activeIndex, steps, onNavigate }: OnboardingRailProps) {
	return (
		<nav
			className="flex w-[220px] flex-none flex-col gap-1 border-r px-3 py-6"
			style={{ borderColor: 'var(--border-soft)', background: 'var(--bg-surface)' }}
			aria-label="Consecration steps"
			data-testid="onboarding-rail"
		>
			<div className="mb-4 flex items-center gap-2 px-2 text-[13px] font-bold tracking-tight">
				<svg width="18" height="18" viewBox="0 0 24 24" fill="none" aria-hidden="true">
					<path
						d="M4 20L12 4L20 20"
						stroke="var(--primary)"
						strokeWidth="2.4"
						strokeLinecap="square"
					/>
					<path d="M8 14H16" stroke="var(--primary)" strokeWidth="2.4" strokeLinecap="square" />
				</svg>
				Ikenga
			</div>
			{ONBOARDING_STEPS.map((id, index) => {
				const record = steps[id];
				const done = record.status === 'completed' || record.status === 'skipped';
				const on = id === activeStepId;
				const reachable = done || record.status !== 'pending' || index <= activeIndex;
				const copy = RAIL_COPY[id];
				return (
					<button
						key={id}
						type="button"
						disabled={!reachable}
						aria-current={on ? 'step' : undefined}
						onClick={() => onNavigate(id)}
						data-testid="onboarding-rail-step"
						data-step-id={id}
						data-done={done}
						className={cn(
							'flex items-center gap-3 rounded-md px-2 py-2 text-left transition-colors',
							on ? '' : reachable ? 'hover:bg-[var(--bg-raised)]' : 'cursor-not-allowed opacity-50'
						)}
						style={{ background: on ? 'var(--bg-raised)' : 'transparent' }}
					>
						<span
							className="flex h-6 w-6 flex-none items-center justify-center rounded-full text-[11px] font-bold"
							style={{
								background: on ? 'var(--primary)' : done ? 'var(--success)' : 'var(--bg-base)',
								color: on || done ? 'var(--primary-fg, white)' : 'var(--fg-muted)',
								border: on || done ? 'none' : '1px solid var(--border-soft)',
							}}
							aria-hidden="true"
						>
							{done && !on ? '✓' : index + 1}
						</span>
						<span className="min-w-0">
							<span className="block truncate text-[13px] font-semibold">{copy.nm}</span>
							<span
								className="block truncate text-[11px]"
								style={{ color: 'var(--fg-faint)' }}
							>
								{copy.sub}
							</span>
						</span>
					</button>
				);
			})}
			{/* `designs/onboarding.html` `.steprail .foot` — the rail's own,
			    step-independent reassurance. Each step's footer additionally
			    carries a step-specific "change later" link to the exact D-03
			    section (see `footer.tsx`'s `SETTINGS_LINKS`); this one opens
			    Settings › Workspace, which owns "Run consecration again". */}
			<div
				className="mt-auto pt-4 text-[11px] leading-relaxed"
				style={{ borderTop: '1px solid var(--border-soft)', color: 'var(--fg-faint)' }}
			>
				Everything here can be changed later in{' '}
				<span style={{ color: 'var(--fg)', fontWeight: 600 }}>Settings</span>.
				<div className="mt-1.5">
					<Link
						to="/settings/workspace"
						className="underline-offset-2 hover:underline"
						style={{ color: 'var(--primary)' }}
						data-testid="onboarding-rail-settings"
					>
						Open Settings → Workspace
					</Link>
				</div>
			</div>
		</nav>
	);
}
