// Cron expression → plain words, for the D-07 `schedules` list ("cron in
// plain words + expression", designs/system-flows.html?state=schedules).
//
// No scheduler ships with WP-42 (no native `cron_*` commands exist — Round 32
// G-45 / Round 31 code sweep), so this is display-only: it never parses a
// cron expression to compute a next-run time, only to describe it. Standard
// 5-field cron (`minute hour day-of-month month day-of-week`); the agent-ops
// 6-field dialect (`schedule_dialect: '6f'`, seconds-first) is described with
// its seconds field named rather than silently dropped.

const WEEKDAYS = ['Sunday', 'Monday', 'Tuesday', 'Wednesday', 'Thursday', 'Friday', 'Saturday'];

function pad2(n: number): string {
	return n < 10 ? `0${n}` : String(n);
}

function isWildcard(field: string): boolean {
	return field === '*';
}

// `*/N` → N, else null.
function stepValue(field: string): number | null {
	const m = /^\*\/(\d+)$/.exec(field);
	return m ? Number(m[1]) : null;
}

/**
 * Describe a standard 5-field cron expression in plain English. Falls back to
 * `at <expr>` for anything this doesn't recognize rather than guessing wrong —
 * per D-07's "the plain words are derived from the expression, never typed
 * separately" rule, an unrecognized expression must not invent a description.
 */
export function cronToWords(expr: string): string {
	const fields = expr.trim().split(/\s+/);
	if (fields.length !== 5) {
		return `at ${expr}`;
	}
	const [minute, hour, dom, month, dow] = fields;

	if (!isWildcard(dom) || !isWildcard(month)) {
		// Day-of-month / month scheduling isn't in the D-07 contact sheet's
		// vocabulary — describe honestly rather than approximate.
		return `at ${expr}`;
	}

	const minuteStep = stepValue(minute);
	if (minuteStep && isWildcard(hour) && isWildcard(dow)) {
		return `Every ${minuteStep} minute${minuteStep === 1 ? '' : 's'}`;
	}

	const hourStep = stepValue(hour);
	if (minute === '0' && hourStep && isWildcard(dow)) {
		return `Every ${hourStep} hour${hourStep === 1 ? '' : 's'}`;
	}

	if (isWildcard(minute) && isWildcard(hour) && isWildcard(dow)) {
		return 'Every minute';
	}

	const min = Number(minute);
	const hr = Number(hour);
	const hasTime = Number.isFinite(min) && Number.isFinite(hr) && !isWildcard(minute) && !isWildcard(hour);
	if (!hasTime) {
		return `at ${expr}`;
	}
	const time = `${pad2(hr)}:${pad2(min)}`;

	if (isWildcard(dow)) {
		return `Every day at ${time}`;
	}

	const dowNum = Number(dow);
	if (Number.isFinite(dowNum) && dowNum >= 0 && dowNum <= 6) {
		return `Every ${WEEKDAYS[dowNum]} at ${time}`;
	}
	if (/^[1-5](,[1-5])*$/.test(dow)) {
		const names = dow.split(',').map((d) => WEEKDAYS[Number(d)]);
		return `Every ${names.join('/')} at ${time}`;
	}

	// Weekday range, e.g. `1-5` — the common "business hours, weekdays" form.
	const rangeMatch = /^([0-6])-([0-6])$/.exec(dow);
	if (rangeMatch) {
		const start = Number(rangeMatch[1]);
		const end = Number(rangeMatch[2]);
		if (end > start) {
			return `Every ${WEEKDAYS[start]}-${WEEKDAYS[end]} at ${time}`;
		}
	}

	return `at ${expr}`;
}

/**
 * Same, for the agent-ops 6-field dialect (`second minute hour dom month dow`).
 * Only the "seconds field is always 0" common case maps onto the 5-field
 * describer; anything else names itself honestly.
 */
export function cronToWordsDialect(expr: string, dialect: '5f' | '6f' | string): string {
	if (dialect !== '6f') {
		return cronToWords(expr);
	}
	const fields = expr.trim().split(/\s+/);
	if (fields.length !== 6 || fields[0] !== '0') {
		return `at ${expr}`;
	}
	return cronToWords(fields.slice(1).join(' '));
}
