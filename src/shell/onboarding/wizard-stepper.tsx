// <WizardStepper> — D-04 consecration chrome (`designs/onboarding.html`).
//
// Full-window, no frame (D-03..07 §"Shared rules": full-window flows have no
// frame). Layout mirrors the mock's `#wiz`:
//   ┌──────────────────────────────────────────────────────────────┐
//   │ Ikenga            Consecration · Step N of 7             ⋯   │  top bar
//   │▰▰▰▱▱▱▱▱▱  progress fill                                      │  4px
//   ├───────────┬────────────────────────────────────────────────┤
//   │  step     │  <step body — renders its own h1/lede/gloss/    │
//   │  rail     │   acts + content; the ONE place Fraunces        │
//   │ (1..7)    │   appears, via `className="font-display"` on    │
//   │           │   each body's <h1>>                             │
//   ├───────────┴────────────────────────────────────────────────┤
//   │ writes note · iyke line          [Skip]  [Back]  [Continue] │  footer
//   └──────────────────────────────────────────────────────────────┘
//
// Step bodies are render-prop children, same contract as the shipped Phase
// 3/4 wizard: `{ goNext, goBack, skip, goTo, payload, setPayload, record,
// isOptional, isFirst, isLast }`.
//
// `data-state` (G-55 state map): every rendered D-04 state gets
// `data-state="<name>"` on this component's root. Precedence when more than
// one condition could apply: an explicit `stateOverride` prop (a step route
// computes `engine-none` / `offline` itself, since only it knows the
// relevant domain condition) wins; otherwise the resume banner's `resume`
// state wins over the plain step id.

import { useEffect, useMemo, useRef, useState } from 'react';
import { useNavigate } from '@tanstack/react-router';

import {
	ONBOARDING_STEPS,
	type OnboardingStepId,
	type OnboardingStepRecord,
	useShellStore,
} from '@/lib/shell/shell-store';
import { ErrorState } from '@/components/states';
import { Button } from '@/components/ui/button';
import { StatusChip } from '@/components/ui/status-chip';

import { OnboardingFooter } from './footer';
import { OnboardingRail } from './rail';
import { useOnboardingStep } from './use-onboarding-step';

// Header labels — kept short for the top-bar step count line. The rail's own
// labels (`RAIL_COPY` in `rail.tsx`) carry the longer descriptive sub-line.
const STEP_LABELS: Record<OnboardingStepId, string> = {
	welcome: 'Welcome',
	engine: 'Chi',
	project: 'Project',
	equipment: 'Ngwa',
	look: 'Look',
	shortcuts: 'Keys',
	done: 'Done',
};

/** Every extra (non-step) D-04 state this chrome can render, per
 *  `designs/onboarding.html` `STATES` (`daily-address` is WP-39's — the
 *  Project dashboard, not this wizard). */
export type OnboardingChromeState = OnboardingStepId | 'resume' | 'offline' | 'engine-none';

export interface WizardStepChildArgs<P> {
	goNext: () => void;
	goBack: () => void;
	skip: () => void;
	/** Jump to an arbitrary step. Sets `mode: 'edit'` if not already. */
	goTo: (id: OnboardingStepId) => void;
	payload: P | undefined;
	setPayload: (p: P) => void;
	record: OnboardingStepRecord<P>;
	isOptional: boolean;
	isFirst: boolean;
	isLast: boolean;
	/** Register work that must finish before the wizard advances — run by
	 *  BOTH the footer's Continue and a body's inline one (they share
	 *  `goNext`, which runs at most one commit at a time). A throw keeps the
	 *  user on the step and shows the error inline with a Retry. Pass null
	 *  to clear. */
	setBeforeNext: (fn: BeforeNext | null) => void;
}

export type BeforeNext = () => Promise<void> | void;

interface WizardStepperProps<P> {
	stepId: OnboardingStepId;
	/** A step route computes this itself from a domain condition only it
	 *  knows about — `engine.tsx` for `engine-none`, `equipment.tsx` for
	 *  `offline` (registry unreachable). Omitted for the plain-step render. */
	stateOverride?: 'offline' | 'engine-none';
	children: (args: WizardStepChildArgs<P>) => React.ReactNode;
}

// ─── "Resume" (wizard reopened half-done) ──────────────────────────────────
//
// Must be decided once per app session (on the FIRST step render after
// `/onboarding` loads), not re-derived on every step transition — otherwise
// finishing step 1 in one continuous run would immediately look like a
// "resume" the moment `activeIndex` ticks past 0. `route.tsx`'s
// `OnboardingLayout` (mounted once for the whole wizard route subtree) calls
// `primeOnboardingResumeFlag()` before any step renders; `WizardStepper`
// (which remounts per step) only reads the frozen result. Both flags are
// module state rather than store state because they are session-local UI
// posture, not anything worth persisting or mirroring to settings.json.
let resumeFlag: boolean | null = null;
let resumeAcknowledged = false;

export function primeOnboardingResumeFlag(): void {
	if (resumeFlag !== null) return;
	const ob = useShellStore.getState().onboarding;
	resumeFlag =
		ob.mode === 'first_run' &&
		ob.completedAt === null &&
		ob.startedAt !== null &&
		Object.values(ob.steps).some((s) => s.status === 'completed' || s.status === 'skipped');
}

/** Test-only escape hatch — vitest can't otherwise reset the module
 *  singleton between cases that each want a fresh "first render". */
export function __resetOnboardingResumeFlagForTests(): void {
	resumeFlag = null;
	resumeAcknowledged = false;
}

export function WizardStepper<P = unknown>({
	stepId,
	stateOverride,
	children,
}: WizardStepperProps<P>) {
	const navigate = useNavigate();
	const { record, setPayload, markCompleted, markSkipped, isOptional } =
		useOnboardingStep<P>(stepId);

	const activeIndex = useShellStore((s) => s.onboarding.activeIndex);
	const mode = useShellStore((s) => s.onboarding.mode);
	const steps = useShellStore((s) => s.onboarding.steps);
	const startOnboarding = useShellStore((s) => s.startOnboarding);
	const setActiveIndex = useShellStore((s) => s.setOnboardingActiveIndex);
	const enterOnboardingEdit = useShellStore((s) => s.enterOnboardingEdit);
	const finishOnboarding = useShellStore((s) => s.finishOnboarding);
	const resetOnboarding = useShellStore((s) => s.resetOnboarding);

	const myIndex = ONBOARDING_STEPS.indexOf(stepId);
	const isFirst = myIndex === 0;
	const isLast = myIndex === ONBOARDING_STEPS.length - 1;
	const progressPct = ((myIndex + 1) / ONBOARDING_STEPS.length) * 100;

	// Whether to still show the resume banner — frozen per session by
	// `resumeFlag`, dismissed (this session only) by `resumeAcknowledged`.
	const [showResume, setShowResume] = useState(() => resumeFlag === true && !resumeAcknowledged);

	const bodyRef = useRef<HTMLDivElement>(null);
	const beforeNextRef = useRef<BeforeNext | null>(null);
	// One commit at a time: a double-click (or footer + inline Continue) must
	// not run `beforeNext` twice — e.g. two `project_create` calls. The ref
	// guards synchronously; the state drives the busy Continue.
	const committingRef = useRef(false);
	const [committing, setCommitting] = useState(false);
	const [commitError, setCommitError] = useState<string | null>(null);

	useEffect(() => {
		if (myIndex >= 0 && myIndex !== activeIndex) {
			setActiveIndex(myIndex);
		}
		startOnboarding(mode);
	}, [myIndex, activeIndex, setActiveIndex, startOnboarding, mode]);

	// biome-ignore lint/correctness/useExhaustiveDependencies: myIndex is the intentional trigger, not an unused dep.
	useEffect(() => {
		bodyRef.current?.focus();
	}, [myIndex]);

	const dismissResume = () => {
		if (!resumeAcknowledged) {
			resumeAcknowledged = true;
			setShowResume(false);
		}
	};

	const goNext = useMemo(
		() => async () => {
			dismissResume();
			if (committingRef.current) return;
			committingRef.current = true;
			setCommitting(true);
			setCommitError(null);
			try {
				await beforeNextRef.current?.();
			} catch (err) {
				console.warn('[onboarding] step commit failed; staying on step', err);
				setCommitError(commitErrorMessage(err));
				return;
			} finally {
				committingRef.current = false;
				setCommitting(false);
			}
			markCompleted();
			const nextIndex = Math.min(ONBOARDING_STEPS.length - 1, myIndex + 1);
			const nextId = ONBOARDING_STEPS[nextIndex]!;
			setActiveIndex(nextIndex);
			if (isLast) return;
			void navigate({ to: `/onboarding/${nextId}` });
		},
		[isLast, markCompleted, myIndex, navigate, setActiveIndex]
	);

	const goBack = useMemo(
		() => () => {
			dismissResume();
			if (isFirst) return;
			const prevIndex = Math.max(0, myIndex - 1);
			const prevId = ONBOARDING_STEPS[prevIndex]!;
			setActiveIndex(prevIndex);
			void navigate({ to: `/onboarding/${prevId}` });
		},
		[isFirst, myIndex, navigate, setActiveIndex]
	);

	const skip = useMemo(
		() => () => {
			dismissResume();
			if (!isOptional) return;
			markSkipped();
			const nextIndex = Math.min(ONBOARDING_STEPS.length - 1, myIndex + 1);
			const nextId = ONBOARDING_STEPS[nextIndex]!;
			setActiveIndex(nextIndex);
			void navigate({ to: `/onboarding/${nextId}` });
		},
		[isOptional, markSkipped, myIndex, navigate, setActiveIndex]
	);

	const goTo = useMemo(
		() => (id: OnboardingStepId) => {
			dismissResume();
			enterOnboardingEdit(id);
			void navigate({ to: `/onboarding/${id}` });
		},
		[enterOnboardingEdit, navigate]
	);

	const startOver = () => {
		dismissResume();
		resetOnboarding();
		void navigate({ to: '/onboarding/welcome' });
	};

	const childArgs: WizardStepChildArgs<P> = {
		goNext: isLast ? finishOnboarding : () => void goNext(),
		goBack,
		skip,
		goTo,
		payload: record.payload,
		setPayload,
		record,
		isOptional,
		isFirst,
		isLast,
		setBeforeNext: (fn) => {
			beforeNextRef.current = fn;
		},
	};

	const dataState: OnboardingChromeState = stateOverride ?? (showResume ? 'resume' : stepId);

	return (
		<div
			data-testid="wizard-stepper"
			data-state={dataState}
			className="flex h-full min-h-0 flex-col bg-background text-foreground"
		>
			{/* ── Top bar ─────────────────────────────────────────────── */}
			<header
				className="flex items-center justify-between border-b px-6 py-3"
				style={{ borderColor: 'var(--border-soft)' }}
			>
				<div className="inline-flex items-center gap-2.5 text-[15px] font-bold tracking-tight">
					<svg width="20" height="20" viewBox="0 0 24 24" fill="none" aria-hidden="true">
						<path
							d="M4 20L12 4L20 20"
							stroke="var(--primary)"
							strokeWidth="2.4"
							strokeLinecap="square"
						/>
						<path d="M8 14H16" stroke="var(--primary)" strokeWidth="2.4" strokeLinecap="square" />
					</svg>
					Ikenga
					<span className="ml-2 text-xs font-normal" style={{ color: 'var(--fg-faint)' }}>
						Consecration
					</span>
				</div>
				<div
					data-testid="wizard-step-label"
					className="text-xs"
					style={{ color: 'var(--fg-muted)' }}
					aria-current="step"
				>
					Step{' '}
					<span className="font-semibold" style={{ color: 'var(--fg)' }}>
						{myIndex + 1}
					</span>{' '}
					of {ONBOARDING_STEPS.length} · {STEP_LABELS[stepId]}
					{mode === 'edit' && (
						<StatusChip tone="info" className="ml-3">
							Edit
						</StatusChip>
					)}
				</div>
			</header>

			{/* ── Progress rail (thin bar, doubles as top-level progress) ── */}
			<div
				className="h-1 w-full"
				style={{ background: 'var(--bg-raised)' }}
				role="progressbar"
				aria-valuemin={0}
				aria-valuemax={ONBOARDING_STEPS.length}
				aria-valuenow={myIndex + 1}
				aria-label={`Onboarding progress: step ${myIndex + 1} of ${ONBOARDING_STEPS.length}`}
			>
				<div
					data-testid="wizard-progress-fill"
					className="h-full transition-[width] duration-300 ease-out motion-reduce:transition-none"
					style={{ width: `${progressPct}%`, background: 'var(--primary)' }}
				/>
			</div>

			<div className="flex min-h-0 flex-1">
				<OnboardingRail
					activeStepId={stepId}
					activeIndex={activeIndex}
					steps={steps}
					onNavigate={goTo}
				/>

				{/* ── Body — step bodies render here. ────────────────────── */}
				<div
					ref={bodyRef}
					tabIndex={-1}
					className="min-h-0 flex-1 overflow-auto px-16 py-10 focus-visible:outline-none"
				>
					{showResume && stateOverride === undefined && (
						<div
							className="mb-6 flex items-center gap-3 rounded-md border px-4 py-3 text-sm"
							style={{ borderColor: 'var(--border-soft)', background: 'var(--bg-surface)' }}
							data-testid="onboarding-resume-banner"
						>
							<span aria-hidden="true">🕐</span>
							<span className="flex-1">
								<b>You left off here.</b> {describeResume(steps, stepId)} Nothing you already
								answered was lost.
							</span>
							<Button variant="ghost" size="sm" onClick={startOver} data-testid="onboarding-start-over">
								Start over
							</Button>
						</div>
					)}
					{commitError !== null && (
						<ErrorState
							data-state="commit-error"
							data-testid="wizard-commit-error"
							heading="Couldn't save this step"
							body={commitError}
							action={{ label: 'Retry', onClick: () => void goNext() }}
							className="mb-6"
						/>
					)}
					{children(childArgs)}
				</div>
			</div>

			<OnboardingFooter
				stepId={stepId}
				isFirst={isFirst}
				isLast={isLast}
				isOptional={isOptional}
				progressLabel={summariseProgress(steps)}
				onBack={goBack}
				onSkip={skip}
				onNext={childArgs.goNext}
				nextBusy={committing}
			/>
		</div>
	);
}

/** Inline copy for a failed step commit (Tauri rejects with a string). */
export function commitErrorMessage(err: unknown): string {
	if (err instanceof Error && err.message) return err.message;
	if (typeof err === 'string' && err) return err;
	return 'Something went wrong — try again.';
}

const COUNT_WORDS = ['no', 'one', 'two', 'three', 'four', 'five', 'six', 'seven'];

function joinNames(names: string[]): string {
	if (names.length <= 1) return names.join('');
	return `${names.slice(0, -1).join(', ')} and ${names[names.length - 1]}`;
}

/** D-04 `resume` banner middle clause (`designs/onboarding.html`: "Chi,
 *  Project and Welcome are done; three steps remain."), computed from the
 *  real per-step status. "Remain" excludes the step being resumed on, as
 *  in the mock (three ticked, sitting on four, of seven). */
export function describeResume(
	steps: Record<OnboardingStepId, OnboardingStepRecord>,
	currentId: OnboardingStepId
): string {
	const done = ONBOARDING_STEPS.filter(
		(id) => steps[id].status === 'completed' || steps[id].status === 'skipped'
	);
	const remaining = ONBOARDING_STEPS.filter((id) => id !== currentId && !done.includes(id)).length;
	const remainClause = `${COUNT_WORDS[remaining] ?? remaining} step${remaining === 1 ? '' : 's'} remain${remaining === 1 ? 's' : ''}.`;
	if (done.length === 0) return remainClause;
	const names = joinNames(done.map((id) => STEP_LABELS[id]));
	return `${names} ${done.length === 1 ? 'is' : 'are'} done; ${remainClause}`;
}

function summariseProgress(steps: Record<OnboardingStepId, OnboardingStepRecord>): string {
	let done = 0;
	let skipped = 0;
	for (const id of ONBOARDING_STEPS) {
		const r = steps[id];
		if (r.status === 'completed') done++;
		if (r.status === 'skipped') skipped++;
	}
	const parts = [`${done}/${ONBOARDING_STEPS.length} done`];
	if (skipped > 0) parts.push(`${skipped} skipped`);
	return parts.join(' · ');
}
