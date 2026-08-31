// Step 1 — Welcome / system preflight.
//
// Calls `detectSystem()` on mount via TanStack Query and renders the 5+
// checks. Continue is gated on no `fail` rows (warnings OK).
//
// Mirrors the Phase 1 prototype `01-welcome.html`: two columns — a left
// hero (caption + headline + three reassurance bullets) and a right
// aside listing the preflight items as cards. The host wizard chrome
// supplies the progress bar / header / footer.

import { useQuery } from '@tanstack/react-query';
import { openExternalUrl } from '@/lib/transport';

import { LoreTerm } from '@/components/lore/lore-term';
import { Button } from '@/components/ui/button';
import { FeedbackState } from '@/components/ui/feedback-state';
import { Input } from '@/components/ui/input';
import { quoteOfTheDay } from '@/lib/lore';
import { useShellStore } from '@/lib/shell/shell-store';
import {
	type CheckLevel,
	type SystemCheck,
	type SystemReport,
	detectSystem,
} from '@/lib/tauri-cmd';

interface WelcomeBodyProps {
	/** Pass-through from the wizard chrome — re-rendered onto the
	 * Continue button so we can disable it when the preflight has fails. */
	onContinue: () => void;
}

const QUERY_KEY = ['onboarding', 'preflight'] as const;

// Doc links per check id. The Rust side stamps stable ids; if it adds a
// new one, the link falls back to the generic onboarding page.
const DOC_LINKS: Record<string, string> = {
	os: 'https://github.com/ikenga-hq/ikenga/blob/main/docs/system-requirements.md',
	disk: 'https://github.com/ikenga-hq/ikenga/blob/main/docs/system-requirements.md',
	app_data: 'https://github.com/ikenga-hq/ikenga/blob/main/docs/troubleshooting.md',
	vault: 'https://github.com/ikenga-hq/ikenga/blob/main/docs/vault.md',
	claude_projects: 'https://github.com/ikenga-hq/ikenga/blob/main/docs/agents.md',
	network: 'https://github.com/ikenga-hq/ikenga/blob/main/docs/troubleshooting.md',
};

const DEFAULT_DOC = 'https://github.com/ikenga-hq/ikenga/blob/main/docs/onboarding.md';

export function WelcomeBody({ onContinue }: WelcomeBodyProps) {
	const { data, isLoading, isError, error, refetch } = useQuery<SystemReport>({
		queryKey: QUERY_KEY,
		queryFn: detectSystem,
		// Preflight is informational — refetching on focus would just thrash
		// the row colors as the user comes back to the wizard.
		refetchOnWindowFocus: false,
		staleTime: 60_000,
	});

	const hasFail = (data?.checks ?? []).some((c) => c.level === 'fail');
	const welcomeQuote = quoteOfTheDay(new Date(), 'welcome');
	const userName = useShellStore((s) => s.userName);
	const setUserName = useShellStore((s) => s.setUserName);

	return (
		<div className="grid h-full gap-12 lg:grid-cols-2">
			{/* ── Left: hero ──────────────────────────────────────────────── */}
			<section>
				<p
					className="mb-3 text-xs font-semibold uppercase tracking-[0.04em]"
					style={{ color: 'var(--primary)' }}
				>
					<LoreTerm term="Consecration">Consecration</LoreTerm>
				</p>
				<h1 className="mb-4 text-4xl font-bold leading-tight tracking-tight">
					Let's bring your <LoreTerm term="Ikenga">Ikenga</LoreTerm> to life.
				</h1>
				<p
					className="mb-6 max-w-[48ch] text-[15px] leading-[1.55]"
					style={{ color: 'var(--fg-muted)' }}
				>
					Ikenga is a personal seat of strength — your desktop home for AI-augmented work, whatever
					you do. In a few minutes you'll meet your <LoreTerm term="Chi">Chi</LoreTerm>, choose your{' '}
					<LoreTerm term="Obi">Obi</LoreTerm>, and welcome a few{' '}
					<LoreTerm term="Alusi">Alusi</LoreTerm>.
				</p>

				<blockquote
					className="mb-6 border-l-2 pl-4 text-[13.5px] italic leading-snug"
					style={{ borderColor: 'var(--primary)', color: 'var(--fg-muted)' }}
					data-testid="welcome-quote"
				>
					&ldquo;{welcomeQuote.text}&rdquo;
					<footer className="mt-1 not-italic text-[11.5px]" style={{ color: 'var(--fg-faint)' }}>
						— {welcomeQuote.source}
						{welcomeQuote.work ? `, ${welcomeQuote.work}` : ''}
					</footer>
				</blockquote>

				<div className="mb-6">
					<label
						htmlFor="welcome-user-name"
						className="mb-1.5 block text-[12px] font-semibold uppercase tracking-[0.04em]"
						style={{ color: 'var(--fg-muted)' }}
					>
						What should your <LoreTerm term="Chi">Chi</LoreTerm> call you?
					</label>
					<Input
						id="welcome-user-name"
						type="text"
						value={userName}
						onChange={(e) => setUserName(e.target.value)}
						placeholder="e.g. Adaeze"
						autoComplete="given-name"
						className="h-9 max-w-[28ch]"
						data-testid="welcome-user-name"
					/>
					<p className="mt-1.5 text-[11.5px]" style={{ color: 'var(--fg-faint)' }}>
						Optional. Used in greetings — you can change it later from Settings.
					</p>
				</div>

				<div className="grid gap-3">
					<Bullet
						title="No cloud account required to start."
						meta="Connect Supabase, Resend & co. only if a package needs them."
					/>
					<Bullet
						title="Local-first — your files stay on this machine."
						meta="Secrets sit in Stronghold, never in env files."
					/>
					<Bullet
						title="Skip any step you don't need."
						meta="We'll surface the gap in Settings if it matters later."
					/>
				</div>
			</section>

			{/* ── Right: preflight ────────────────────────────────────────── */}
			<aside>
				<div className="mb-3 flex items-center justify-between">
					<p
						className="text-xs font-semibold uppercase tracking-[0.04em]"
						style={{ color: 'var(--fg-muted)' }}
					>
						System preflight
					</p>
					{!isLoading && (
						<button
							type="button"
							onClick={() => refetch()}
							className="text-xs underline-offset-2 hover:underline"
							style={{ color: 'var(--fg-faint)' }}
							data-testid="preflight-rerun"
						>
							Re-check
						</button>
					)}
				</div>

				{isLoading && (
					<div data-testid="preflight-loading">
						<FeedbackState
							variant="loading"
							heading="Detecting system…"
							fill={false}
							className="min-h-0 py-4"
						/>
					</div>
				)}

				{isError && (
					<div data-testid="preflight-error">
						<FeedbackState
							variant="error"
							heading="Detection failed"
							body={String((error as Error)?.message ?? error)}
							action={
								<Button size="sm" onClick={() => void refetch()}>
									Retry
								</Button>
							}
							fill={false}
							className="min-h-0 py-4"
						/>
					</div>
				)}

				{!isLoading && !isError && data && (
					<div className="grid gap-3" data-testid="preflight-list">
						{data.checks.map((check) => (
							<PreflightRow key={check.id} check={check} />
						))}
					</div>
				)}

				{/* In-body Continue mirrors the footer button but is reachable
				    without scrolling on small windows; the wizard chrome's
				    Continue stays the canonical action target. */}
				<div className="mt-6 flex items-center justify-end gap-3">
					<Button
						onClick={onContinue}
						disabled={isLoading || hasFail}
						data-testid="welcome-inline-continue"
					>
						{hasFail ? 'Resolve the failing check above' : 'Looks good — continue'}
					</Button>
				</div>
			</aside>
		</div>
	);
}

function Bullet({ title, meta }: { title: string; meta: string }) {
	return (
		<div className="flex items-start gap-3">
			<div
				className="mt-0.5 flex h-5 w-5 flex-none items-center justify-center rounded-full text-[11px] font-bold"
				style={{ background: 'var(--success)', color: 'var(--success-fg, white)' }}
				aria-hidden="true"
			>
				✓
			</div>
			<div>
				<div className="text-[13px] font-semibold">{title}</div>
				<div className="mt-0.5 text-xs" style={{ color: 'var(--fg-muted)' }}>
					{meta}
				</div>
			</div>
		</div>
	);
}

function PreflightRow({ check }: { check: SystemCheck }) {
	const tone = toneFor(check.level);
	return (
		<div
			className="grid grid-cols-[24px_1fr_auto] items-center gap-4 rounded-md border px-5 py-4"
			style={{ borderColor: tone.border, background: 'var(--bg-surface)' }}
			data-testid="preflight-row"
			data-level={check.level}
			data-check-id={check.id}
		>
			<div
				className="flex h-6 w-6 items-center justify-center rounded-full text-xs font-bold"
				style={{ background: tone.glyphBg, color: tone.glyphFg }}
				aria-hidden="true"
			>
				{tone.glyph}
			</div>
			<div className="min-w-0">
				<div className="truncate text-[13px] font-semibold">{labelFor(check.id)}</div>
				<div
					className="mt-0.5 truncate text-xs"
					style={{ color: 'var(--fg-muted)' }}
					title={check.message}
				>
					{check.message}
				</div>
				{check.fix_hint && check.level !== 'pass' && (
					<div className="mt-1 flex items-center gap-2 text-xs" style={{ color: tone.fg }}>
						<span>{check.fix_hint}</span>
						<button
							type="button"
							onClick={() => {
								const url = DOC_LINKS[check.id] ?? DEFAULT_DOC;
								void openExternalUrl(url).catch(() => {});
							}}
							className="underline underline-offset-2"
							data-testid="preflight-help"
						>
							Help
						</button>
					</div>
				)}
			</div>
			<span className="font-mono text-[11.5px] uppercase tracking-wider" style={{ color: tone.fg }}>
				{check.level}
			</span>
		</div>
	);
}

function labelFor(id: string): string {
	switch (id) {
		case 'os':
			return 'Operating system';
		case 'disk':
			return 'Disk space';
		case 'app_data':
			return 'App data dir';
		case 'vault':
			return 'Stronghold vault';
		case 'claude_projects':
			return 'Claude projects dir';
		case 'network':
			return 'Network connectivity';
		default:
			return id;
	}
}

interface Tone {
	border: string;
	fg: string;
	glyph: string;
	glyphBg: string;
	glyphFg: string;
}

function toneFor(level: CheckLevel): Tone {
	if (level === 'fail') {
		return {
			border: 'var(--danger)',
			fg: 'var(--danger)',
			glyph: '×',
			glyphBg: 'var(--danger)',
			glyphFg: 'var(--danger-fg, white)',
		};
	}
	if (level === 'warn') {
		return {
			border: 'var(--warning, var(--border-strong))',
			fg: 'var(--warning, var(--fg-muted))',
			glyph: '!',
			glyphBg: 'var(--warning, var(--bg-raised))',
			glyphFg: 'var(--warning-fg, var(--fg))',
		};
	}
	return {
		border: 'var(--border-soft)',
		fg: 'var(--success)',
		glyph: '✓',
		glyphBg: 'var(--success)',
		glyphFg: 'var(--success-fg, white)',
	};
}

/** Exported for tests — pure decision on whether Continue should be
 *  enabled given a preflight report. Keeps the rule in one place. */
export function canContinueFromPreflight(report: SystemReport | undefined): boolean {
	if (!report) return false;
	return !report.checks.some((c) => c.level === 'fail');
}
