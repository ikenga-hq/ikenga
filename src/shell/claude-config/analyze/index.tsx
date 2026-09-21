// Analyze surfaces router (Phase 4). Routes the Ngwa ANALYZE sidebar surfaces
// to their view. All five are live: capability graph (D-06), store map (D-07),
// hook lifecycle + inventory/health + orchestration flow (D-03). Flow is
// regex-derived from primitive bodies (@/lib/claude-graph/flow).

import type { ClaudeConfig, ClaudeStoreEntry } from '@/lib/tauri-cmd';
import type { EngineConfigItem, NgwaSurfaceId } from '../ngwa-surface';
import { FlowView } from './flow-view';
import { GraphView } from './graph-view';
import { HealthView } from './health-view';
import { LifecycleView } from './lifecycle-view';
import { StoreMap } from './store-map';

interface AnalyzeSurfaceProps {
	surface: NgwaSurfaceId;
	config: ClaudeConfig | null;
	scope: string;
	items: EngineConfigItem[];
	store: ClaudeStoreEntry[];
}

export function AnalyzeSurface({ surface, config, scope, items, store }: AnalyzeSurfaceProps) {
	if (surface === 'graph') return <GraphView config={config} scope={scope} />;
	if (surface === 'map') return <StoreMap items={items} store={store} />;
	if (surface === 'life') return <LifecycleView config={config} scope={scope} />;
	if (surface === 'health') return <HealthView items={items} store={store} scope={scope} />;
	if (surface === 'flow') return <FlowView config={config} />;

	return <div className="ngwa-analyze-empty">Unknown analyze surface.</div>;
}
