// The status bar's compact connection indicator: a dot and "340 ms", amber
// past 150 ms and red past 300 ms (or when the server stopped answering),
// with a tooltip that says what the number means for typing. Browser tabs
// only; it renders nothing until the first measurement, and nothing at all
// on the desktop (see `useConnectionRtt`).

import { cn } from '@/components/ui/utils';
import { type RttLevel, type RttSummary, explainRtt, formatRtt } from '@/lib/connection/rtt';

export const CONNECTION_ROUTE = '/ngwa/health?section=connection';

const DOT: Record<RttLevel, string> = {
	good: 'bg-[var(--success)]',
	warn: 'bg-[var(--warning)]',
	bad: 'bg-[var(--danger)]',
	unknown: 'bg-muted-foreground/50',
};

const TEXT: Record<RttLevel, string> = {
	good: '',
	warn: 'text-[var(--warning)]',
	bad: 'text-[var(--danger)]',
	unknown: '',
};

/** Whether the segment has anything to say yet. */
export function hasConnectionReading(s: RttSummary): boolean {
	return s.count > 0 || s.failures > 0;
}

/** Short figure for the bar: "340 ms", or "no reply" when stalled. */
export function shortRtt(s: RttSummary): string {
	if (s.failures >= 2) return 'no reply';
	return s.medianMs === null ? '…' : `${Math.round(s.medianMs)} ms`;
}

export function connectionTitle(s: RttSummary): string {
	return `Connection: ${formatRtt(s)}. ${explainRtt(s)}`;
}

/** The segment's content; the status bar wraps it in its own button. */
export function ConnectionSegmentBody({ summary }: { summary: RttSummary }) {
	return (
		<>
			<span aria-hidden className={cn('h-1.5 w-1.5 rounded-full', DOT[summary.level])} />
			<span className={cn('font-mono', TEXT[summary.level])}>{shortRtt(summary)}</span>
		</>
	);
}
