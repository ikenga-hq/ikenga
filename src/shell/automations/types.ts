// D-07 `schedules` — Automations full view (designs/system-flows.html?state=schedules).
//
// One row shape across three sources. No execution engine is built here
// (Round 32 G-45 / Round 31 code sweep: no native `cron_*` commands exist) —
// `agent-ops` is the only source with a live daemon behind it, so it is the
// only source where pause / run now / edit / delete are real. The other two
// are read-only reflections of a manifest; every disabled control here
// carries the reason as its `title` per `06-interaction-spec.md` §1.2.

export type AutomationSource = 'agent-ops' | 'manifest-cron' | 'workflow';

export interface AutomationRow {
	/** Stable across refetches — used as the React key and for row selection. */
	id: string;
	source: AutomationSource;
	name: string;
	/** Plain-words description; always derived from `cronExpr`, never typed
	 *  separately (D-07 rule). `null` for a workflow with no schedule trigger. */
	cronWords: string | null;
	cronExpr: string | null;
	/** IANA timezone the schedule runs in. Round-tripped from the daemon's own
	 *  record (`AgentOpsRawJob.timezone`) so editing and saving a job never
	 *  silently rewrites it to UTC (WP-42-F0). `'UTC'` for read-only sources
	 *  that carry no timezone concept. */
	timezone: string;
	/** What runs — e.g. `skill · release-status`, `shell · build.sh`, `workflow · Nightly Build`. */
	target: string;
	/** Engine id, or `—` for a bare shell command / no-engine source. */
	engine: string;
	lastRun: string;
	nextRun: string;
	/** File the row's definition lives in, shown per the "file path shown" DoD line. */
	filePath: string;
	paused: boolean;
	/** `null` = the action is live; a string names why it is disabled (used
	 *  verbatim as the control's `title`, per §1.2). */
	runNowDisabledReason: string | null;
	pauseDisabledReason: string | null;
	editDisabledReason: string | null;
	deleteDisabledReason: string | null;
	/** Only `agent-ops` rows carry a job id the existing pkg/iyke commands
	 *  (`agent_ops_*`) accept. */
	agentOpsJobId: string | null;
}

export const READ_ONLY_MANIFEST_REASON =
	"Declared in a package manifest — it's read-only here because the manifest wins on the next reload.";

export const NO_RUNNER_REASON =
	'No execution engine is wired for this source — the shell only observes the agent-ops daemon (Round 32 G-45).';
