// The status bar's compact server-health chip: one dot and a short label
// ("Server", or the worst reason: "Memory 88%"). Admins in a browser tab
// only (`mayAskForServerHealth`). It reads the SAME query as the Settings
// card (`useServerHealth`, shared key), polling every 60 s, and only while
// the tab is visible. A failed read is a grey dot, never a toast. The
// judgement is `assess()`: one set of thresholds for card and chip.

import { cn } from '@/components/ui/utils';
import {
	mayAskForServerHealth,
	SERVER_HEALTH_CHIP_POLL_MS,
	useServerHealth,
} from '@/lib/queries/server-health';
import {
	assess,
	type Chip,
	fmtBytes,
	type Level,
	memoryUsedPct,
	type ServerHealth,
} from '@/lib/server-health/model';

/** The card lives in Settings › About (pane routes drop the `#server-health` hash). */
export const SERVER_HEALTH_ROUTE = '/settings/about';

export interface HealthChipView {
	level: Level;
	/** Short text beside the dot. */
	label: string;
	/** Full sentence for the accessible name. */
	aria: string;
	/** Hover summary: CPU, memory, disk, last backup. */
	tooltip: string;
}

const DOT: Record<Level, string> = {
	ok: 'bg-[var(--success)]',
	warn: 'bg-[var(--warning)]',
	error: 'bg-[var(--danger)]',
	unknown: 'bg-muted-foreground/50',
};
const TEXT: Record<Level, string> = {
	ok: '',
	warn: 'text-[var(--warning)]',
	error: 'text-[var(--danger)]',
	unknown: '',
};
const WORD: Record<Level, string> = {
	ok: 'OK',
	warn: 'Warning',
	error: 'Critical',
	unknown: 'Unknown',
};

function reason(c: Chip, h: ServerHealth): string {
	switch (c.id) {
		case 'memory':
			return h.memory ? `Memory ${memoryUsedPct(h.memory)}%` : 'Memory';
		case 'disk': {
			const d = h.disk;
			const used =
				d && d.total_bytes > 0 ? Math.round((1 - d.free_bytes / d.total_bytes) * 100) : null;
			return used === null ? 'Disk' : `Disk ${used}%`;
		}
		case 'backups':
			return c.level === 'error' ? 'Backup failed' : 'Backup overdue';
		case 'load':
			return 'Load high';
		case 'pressure':
			return 'Memory pressure';
		case 'units':
			return c.level === 'error' ? 'Service failed' : 'Service stopped';
		default:
			return c.label;
	}
}

function lastBackup(h: ServerHealth): string {
	const times = (h.backups?.databases ?? [])
		.map((d) => d.last_success)
		.filter((t): t is string => !!t)
		.sort();
	const last = times[times.length - 1];
	return last ? last.replace('T', ' ').replace('Z', ' UTC') : 'none recorded';
}

function summary(h: ServerHealth): string {
	const parts: string[] = [];
	if (h.load)
		parts.push(`CPU load ${h.load.m5.toFixed(2)}${h.cpu_count ? ` on ${h.cpu_count} cores` : ''}`);
	if (h.memory) parts.push(`memory ${memoryUsedPct(h.memory)}% used`);
	if (h.disk && h.disk.total_bytes > 0) {
		parts.push(
			`disk ${Math.round((1 - h.disk.free_bytes / h.disk.total_bytes) * 100)}% used (${fmtBytes(h.disk.free_bytes)} free)`
		);
	}
	parts.push(`last backup ${lastBackup(h)}`);
	return parts.join(', ');
}

/** Pure mapping from a snapshot (or its absence) to what the chip shows. */
export function summarizeHealth(
	h: ServerHealth | null | undefined,
	nowMs = Date.now()
): HealthChipView {
	if (!h) {
		return {
			level: 'unknown',
			label: 'Server',
			aria: 'Server health unknown. Open Settings, About.',
			tooltip: 'Server health could not be read.',
		};
	}
	const { chips, overall } = assess(h, nowMs);
	const details = summary(h);
	if (overall === 'ok' || overall === 'unknown') {
		const level: Level = overall;
		return {
			level,
			label: 'Server',
			aria: `Server ${WORD[level].toLowerCase()}: ${details}. Open Settings, About.`,
			tooltip: `Server ${WORD[level].toLowerCase()}: ${details}`,
		};
	}
	const worst = chips.find((c) => c.level === overall) as Chip;
	const label = reason(worst, h);
	return {
		level: overall,
		label,
		aria: `Server ${WORD[overall].toLowerCase()}: ${label}. ${details}. Open Settings, About.`,
		tooltip: `${label}${worst.note ? `. ${worst.note}` : ''}\n${details}`,
	};
}

/** `null` unless this tab may see it (browser, admin). */
export function useServerHealthChip(): HealthChipView | null {
	const q = useServerHealth({ pollMs: SERVER_HEALTH_CHIP_POLL_MS });
	if (!mayAskForServerHealth()) return null;
	// Failed first read, or an unreadable (newer-schema) snapshot: grey, silent.
	return summarizeHealth(q.data ?? null);
}

export function ServerHealthChipBody({ chip }: { chip: HealthChipView }) {
	return (
		<>
			<span aria-hidden className={cn('h-1.5 w-1.5 rounded-full', DOT[chip.level])} />
			<span className={TEXT[chip.level]}>{chip.label}</span>
		</>
	);
}
