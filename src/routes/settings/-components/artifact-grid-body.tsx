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
import { writeSettingsField } from '@/lib/settings/client';
import type { SettingsWriteOptions } from '@/lib/settings/types';
import { SettingsFieldRow, useSettingsSection } from '@/shell/settings/field';

const ARTIFACT_GRID_GLOBAL_QK = ['settings', 'artifact-grid', 'global'] as const;
const HANDOFF_PREF_QK = ['settings', 'artifact-grid', 'handoff-pref'] as const;

// Schema destination for these three fields: `workspace.artifact.{defaultSink,
// stackMode, terminalHandoff}` (drafts/settings-schema.md §2.1). Personal
// scope keeps writing the legacy settings_kv rows directly (the same rows
// the file-backed cache mirrors back into `effective.workspace.artifact`),
// matching the pattern `engines.tsx`'s TerminalSectionBody uses for
// engines.agentEnvironment / .agentWslDistro / .resumeTerminals. Project
// scope writes through the settings client so these fields get a project
// override marker + revert like every other D-03 field.
interface ArtifactFieldValueMap {
	'workspace.artifact.defaultSink': DefaultSink;
	'workspace.artifact.stackMode': StackMode;
	'workspace.artifact.terminalHandoff': HandoffPref;
}

export function ArtifactGridSectionBody() {
	const qc = useQueryClient();
	const { scope, projectId, result, refresh } = useSettingsSection();
	const isProject = scope === 'project';
	const artifactOverride = result?.effective.workspace?.artifact;

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
		enabled: !isProject,
	});

	const handoffQ = useQuery({
		queryKey: HANDOFF_PREF_QK,
		queryFn: loadHandoffPref,
		staleTime: 10_000,
		enabled: !isProject,
	});

	async function writeArtifact<K extends keyof ArtifactFieldValueMap>(
		field: K,
		value: ArtifactFieldValueMap[K]
	) {
		if (!projectId) return;
		await writeSettingsField({ scope: 'project', field, value, projectId } as SettingsWriteOptions);
		refresh();
	}

	const onSink = async (v: DefaultSink) => {
		if (isProject) {
			await writeArtifact('workspace.artifact.defaultSink', v);
			return;
		}
		await setGlobalDefaultSink(v);
		qc.invalidateQueries({ queryKey: ARTIFACT_GRID_GLOBAL_QK });
	};
	const onStack = async (v: StackMode) => {
		if (isProject) {
			await writeArtifact('workspace.artifact.stackMode', v);
			return;
		}
		await setGlobalStackMode(v);
		qc.invalidateQueries({ queryKey: ARTIFACT_GRID_GLOBAL_QK });
	};
	const onHandoff = async (v: HandoffPref) => {
		if (isProject) {
			await writeArtifact('workspace.artifact.terminalHandoff', v);
			return;
		}
		await saveHandoffPref(v);
		qc.invalidateQueries({ queryKey: HANDOFF_PREF_QK });
	};

	const sink = (isProject ? artifactOverride?.defaultSink : q.data?.defaultSink) ?? 'auto';
	const stack = (isProject ? artifactOverride?.stackMode : q.data?.stackMode) ?? 'collapsed';
	const handoff = (isProject ? artifactOverride?.terminalHandoff : handoffQ.data) ?? 'ask';

	return (
		<>
			<section className="overflow-hidden rounded-lg border border-[var(--border-soft)] bg-card">
				<header className="border-b border-[var(--border-soft)] bg-[var(--bg-sunken)] px-4 py-2.5">
					<h3 className="text-xs font-semibold uppercase tracking-wider text-muted-foreground">
						Artifact routing
					</h3>
				</header>
				<SettingsFieldRow
					field="workspace.artifact.defaultSink"
					label="Default sink"
					desc="Where pin clicks dispatch when no override is set. Auto picks the foreground claude PTY if one exists, falling back to the side-pane terminal."
					className="[&>div:first-child]:pl-4"
				>
					<SegmentedSink value={sink} onChange={(v) => void onSink(v)} />
				</SettingsFieldRow>
			</section>

			<section className="overflow-hidden rounded-lg border border-[var(--border-soft)] bg-card">
				<header className="border-b border-[var(--border-soft)] bg-[var(--bg-sunken)] px-4 py-2.5">
					<h3 className="text-xs font-semibold uppercase tracking-wider text-muted-foreground">
						Artifact layout
					</h3>
				</header>
				<SettingsFieldRow
					field="workspace.artifact.stackMode"
					label="Stack mode"
					desc="Whether variant stacks (an artifact and its sibling folder of variants) open expanded by default. Toggleable per stack at runtime."
					className="[&>div:first-child]:pl-4"
				>
					<SegmentedStackMode value={stack} onChange={(v) => void onStack(v)} />
				</SettingsFieldRow>
			</section>

			<section className="overflow-hidden rounded-lg border border-[var(--border-soft)] bg-card">
				<header className="border-b border-[var(--border-soft)] bg-[var(--bg-sunken)] px-4 py-2.5">
					<h3 className="text-xs font-semibold uppercase tracking-wider text-muted-foreground">
						Artifact wizard
					</h3>
				</header>
				<SettingsFieldRow
					field="workspace.artifact.terminalHandoff"
					label="Terminal handoff"
					desc="When the wizard's Studio swaps from grid to loupe (the agent wrote a file), what to do with the wizard's terminal pane. Attach moves it into the loupe's Terminal tab; Keep leaves it in the right pane; Ask shows a modal each time."
					className="[&>div:first-child]:pl-4"
				>
					<SegmentedHandoff value={handoff} onChange={(v) => void onHandoff(v)} />
				</SettingsFieldRow>
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
