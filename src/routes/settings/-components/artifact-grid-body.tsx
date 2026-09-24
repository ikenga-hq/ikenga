import { useQuery, useQueryClient } from '@tanstack/react-query';

import {
	type DefaultSink,
	type StackMode,
	GLOBAL_KEYS,
	parseDefaultSink,
	setGlobalDefaultSink,
	setGlobalStackMode,
} from '@/shell/artifact-studio/grid-settings';
import { settingsGet } from '@/lib/tauri-cmd';
import {
	type HandoffPref,
	loadHandoffPref,
	saveHandoffPref,
} from '@/shell/artifact-wizard/handoff-pref';

import { SettingRow } from './setting-row';

const ARTIFACT_GRID_GLOBAL_QK = ['settings', 'artifact-grid', 'global'] as const;
const HANDOFF_PREF_QK = ['settings', 'artifact-grid', 'handoff-pref'] as const;

export function ArtifactGridSectionBody() {
	const qc = useQueryClient();

	const q = useQuery({
		queryKey: ARTIFACT_GRID_GLOBAL_QK,
		queryFn: async () => {
			const [sink, stack] = await Promise.all([
				settingsGet(GLOBAL_KEYS.defaultSink),
				settingsGet(GLOBAL_KEYS.stackMode),
			]);
			return {
				defaultSink: parseDefaultSink(sink) ?? 'auto',
				stackMode: (stack === 'expanded' ? 'expanded' : 'collapsed') as StackMode,
			};
		},
		staleTime: 10_000,
	});

	const handoffQ = useQuery({
		queryKey: HANDOFF_PREF_QK,
		queryFn: loadHandoffPref,
		staleTime: 10_000,
	});

	const onSink = async (v: DefaultSink) => {
		await setGlobalDefaultSink(v);
		qc.invalidateQueries({ queryKey: ARTIFACT_GRID_GLOBAL_QK });
	};
	const onStack = async (v: StackMode) => {
		await setGlobalStackMode(v);
		qc.invalidateQueries({ queryKey: ARTIFACT_GRID_GLOBAL_QK });
	};
	const onHandoff = async (v: HandoffPref) => {
		await saveHandoffPref(v);
		qc.invalidateQueries({ queryKey: HANDOFF_PREF_QK });
	};

	const sink = q.data?.defaultSink ?? 'auto';
	const stack = q.data?.stackMode ?? 'collapsed';
	const handoff = handoffQ.data ?? 'ask';

	return (
		<>
			<section className="overflow-hidden rounded-lg border border-[var(--border-soft)] bg-card">
				<header className="border-b border-[var(--border-soft)] bg-[var(--bg-sunken)] px-4 py-2.5">
					<h3 className="text-xs font-semibold uppercase tracking-wider text-muted-foreground">
						Artifact routing
					</h3>
				</header>
				<SettingRow
					label="Default sink"
					desc="Where pin clicks dispatch when no override is set. Auto picks the foreground claude PTY if one exists, falling back to the side-pane terminal."
					className="[&>div:first-child]:pl-4"
				>
					<SegmentedSink value={sink} onChange={(v) => void onSink(v)} />
				</SettingRow>
			</section>

			<section className="overflow-hidden rounded-lg border border-[var(--border-soft)] bg-card">
				<header className="border-b border-[var(--border-soft)] bg-[var(--bg-sunken)] px-4 py-2.5">
					<h3 className="text-xs font-semibold uppercase tracking-wider text-muted-foreground">
						Artifact layout
					</h3>
				</header>
				<SettingRow
					label="Stack mode"
					desc="Whether variant stacks (an artifact and its sibling folder of variants) open expanded by default. Toggleable per stack at runtime."
					className="[&>div:first-child]:pl-4"
				>
					<SegmentedStackMode value={stack} onChange={(v) => void onStack(v)} />
				</SettingRow>
			</section>

			<section className="overflow-hidden rounded-lg border border-[var(--border-soft)] bg-card">
				<header className="border-b border-[var(--border-soft)] bg-[var(--bg-sunken)] px-4 py-2.5">
					<h3 className="text-xs font-semibold uppercase tracking-wider text-muted-foreground">
						Artifact wizard
					</h3>
				</header>
				<SettingRow
					label="Terminal handoff"
					desc="When the wizard's Studio swaps from grid to loupe (the agent wrote a file), what to do with the wizard's terminal pane. Attach moves it into the loupe's Terminal tab; Keep leaves it in the right pane; Ask shows a modal each time."
					className="[&>div:first-child]:pl-4"
				>
					<SegmentedHandoff value={handoff} onChange={(v) => void onHandoff(v)} />
				</SettingRow>
			</section>
		</>
	);
}

function SegmentedHandoff({
	value,
	onChange,
}: {
	value: HandoffPref;
	onChange: (v: HandoffPref) => void;
}) {
	return (
		<div className="inline-flex overflow-hidden rounded border border-border">
			{(['ask', 'attach', 'keep'] as const).map((opt) => (
				<button
					key={opt}
					type="button"
					onClick={() => onChange(opt)}
					className={
						'cursor-pointer border-r border-border px-3 py-1 text-xs capitalize last:border-r-0 ' +
						(value === opt
							? 'bg-foreground/10 text-foreground'
							: 'text-muted-foreground hover:text-foreground')
					}
				>
					{opt}
				</button>
			))}
		</div>
	);
}

function SegmentedSink({
	value,
	onChange,
}: {
	value: DefaultSink;
	onChange: (v: DefaultSink) => void;
}) {
	return (
		<div className="inline-flex overflow-hidden rounded border border-border">
			{(['auto', 'terminal', 'chi', 'clipboard'] as const).map((opt) => (
				<button
					key={opt}
					type="button"
					onClick={() => onChange(opt)}
					className={
						'cursor-pointer border-r border-border px-3 py-1 text-xs capitalize last:border-r-0 ' +
						(value === opt
							? 'bg-foreground/10 text-foreground'
							: 'text-muted-foreground hover:text-foreground')
					}
				>
					{opt}
				</button>
			))}
		</div>
	);
}

function SegmentedStackMode({
	value,
	onChange,
}: {
	value: StackMode;
	onChange: (v: StackMode) => void;
}) {
	return (
		<div className="inline-flex overflow-hidden rounded border border-border">
			{(['collapsed', 'expanded'] as const).map((opt) => (
				<button
					key={opt}
					type="button"
					onClick={() => onChange(opt)}
					className={
						'cursor-pointer border-r border-border px-3 py-1 text-xs capitalize last:border-r-0 ' +
						(value === opt
							? 'bg-foreground/10 text-foreground'
							: 'text-muted-foreground hover:text-foreground')
					}
				>
					{opt}
				</button>
			))}
		</div>
	);
}
