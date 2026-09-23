import { useMemo } from 'react';
import {
  Activity,
  Bot,
  CheckCircle2,
  Clock,
  Command,
  GitBranch,
  Layers,
  Play,
  ShieldAlert,
  Wrench,
  Zap,
} from 'lucide-react';
import type { WorkflowGraph, WorkflowNode, WorkflowNodeKind } from '@/lib/workflows/graph';
import { getTopologicalOrder } from '@/lib/workflows/graph';

export interface NgwaFlowRendererProps {
  graph: WorkflowGraph;
  activeNodeId?: string | null;
  onSelectNode?: (nodeId: string) => void;
  onRunNode?: (nodeId: string) => void;
  className?: string;
}

function getNodeIcon(kind: WorkflowNodeKind) {
  switch (kind) {
    case 'phase':
      return Layers;
    case 'step':
      return CheckCircle2;
    case 'agent':
      return Bot;
    case 'skill':
      return Zap;
    case 'command':
      return Command;
    case 'tool':
      return Wrench;
    case 'gate':
      return ShieldAlert;
    case 'schedule':
      return Clock;
    default:
      return Activity;
  }
}

function getNodeKindBadgeClass(kind: WorkflowNodeKind): string {
  switch (kind) {
    case 'phase':
      return 'bg-blue-500/10 text-blue-500 border-blue-500/20';
    case 'step':
      return 'bg-green-500/10 text-green-500 border-green-500/20';
    case 'agent':
      return 'bg-purple-500/10 text-purple-500 border-purple-500/20';
    case 'skill':
      return 'bg-amber-500/10 text-amber-500 border-amber-500/20';
    case 'command':
      return 'bg-sky-500/10 text-sky-500 border-sky-500/20';
    case 'tool':
      return 'bg-emerald-500/10 text-emerald-500 border-emerald-500/20';
    case 'gate':
      return 'bg-rose-500/10 text-rose-500 border-rose-500/20';
    case 'schedule':
      return 'bg-orange-500/10 text-orange-500 border-orange-500/20';
    default:
      return 'bg-muted text-muted-foreground border-border';
  }
}

export function NgwaFlowRenderer({
  graph,
  activeNodeId,
  onSelectNode,
  onRunNode,
  className = '',
}: NgwaFlowRendererProps) {
  const nodeMap = useMemo(() => {
    const map = new Map<string, WorkflowNode>();
    for (const node of graph.nodes) {
      map.set(node.id, node);
    }
    return map;
  }, [graph.nodes]);

  const sortedNodeIds = useMemo(() => {
    try {
      return getTopologicalOrder(graph);
    } catch {
      return graph.nodes.map((n) => n.id);
    }
  }, [graph]);

  // Group nodes into phases or topological sequence
  const phases = useMemo(() => {
    const phaseList: Array<{ id: string; title: string; nodes: WorkflowNode[] }> = [];
    let currentPhase: { id: string; title: string; nodes: WorkflowNode[] } = {
      id: 'default',
      title: 'Workflow Execution Flow',
      nodes: [],
    };

    for (const id of sortedNodeIds) {
      const node = nodeMap.get(id);
      if (!node) continue;

      if (node.kind === 'phase') {
        if (currentPhase.nodes.length > 0) {
          phaseList.push(currentPhase);
        }
        currentPhase = {
          id: node.id,
          title: node.label,
          nodes: [],
        };
      } else {
        currentPhase.nodes.push(node);
      }
    }

    if (currentPhase.nodes.length > 0 || phaseList.length === 0) {
      phaseList.push(currentPhase);
    }

    return phaseList;
  }, [sortedNodeIds, nodeMap]);

  if (graph.nodes.length === 0) {
    return (
      <div className={`p-8 text-center border rounded-lg bg-card/50 ${className}`}>
        <GitBranch className="h-8 w-8 mx-auto text-muted-foreground mb-2" />
        <h3 className="text-sm font-semibold">Empty Workflow Graph</h3>
        <p className="text-xs text-muted-foreground mt-1">
          No steps or phases declared in this workflow.
        </p>
      </div>
    );
  }

  return (
    <div className={`space-y-6 ${className}`}>
      {/* Workflow Header */}
      <div className="flex items-center justify-between border-b pb-4">
        <div>
          <div className="flex items-center gap-2">
            <h2 className="text-base font-semibold tracking-tight">{graph.title}</h2>
            <span className="text-[10px] uppercase font-mono px-1.5 py-0.5 rounded border bg-muted/60 text-muted-foreground">
              {graph.source}
            </span>
          </div>
          {graph.source_path && (
            <p className="text-xs text-muted-foreground font-mono mt-0.5 truncate">
              {graph.source_path}
            </p>
          )}
        </div>
        <div className="flex items-center gap-4 text-xs text-muted-foreground font-mono">
          <span>{graph.nodes.length} nodes</span>
          <span>{graph.edges.length} edges</span>
        </div>
      </div>

      {/* Visual Workflow Flow */}
      <div className="space-y-6">
        {phases.map((phase) => (
          <div key={phase.id} className="rounded-lg border bg-card p-4 space-y-3">
            <div className="flex items-center gap-2 text-xs font-semibold text-foreground/80">
              <Layers className="h-3.5 w-3.5 text-primary" />
              <span>{phase.title}</span>
            </div>

            <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-3">
              {phase.nodes.map((node) => {
                const Icon = getNodeIcon(node.kind);
                const badgeClass = getNodeKindBadgeClass(node.kind);
                const isSelected = activeNodeId === node.id;

                const incomingEdges = graph.edges.filter((e) => e.to === node.id);
                const outgoingEdges = graph.edges.filter((e) => e.from === node.id);

                return (
                  <div
                    key={node.id}
                    onClick={() => onSelectNode?.(node.id)}
                    className={`rounded-md border p-3 flex flex-col justify-between transition-all cursor-pointer ${
                      isSelected
                        ? 'border-primary ring-1 ring-primary bg-primary/5'
                        : 'border-border/60 hover:border-border bg-card/80 hover:bg-card'
                    }`}
                  >
                    <div className="space-y-2">
                      <div className="flex items-center justify-between gap-2">
                        <span
                          className={`inline-flex items-center gap-1 text-[10px] font-mono px-1.5 py-0.5 rounded-full border ${badgeClass}`}
                        >
                          <Icon className="h-3 w-3" />
                          <span>{node.kind}</span>
                        </span>
                        {node.run && onRunNode && (
                          <button
                            type="button"
                            onClick={(e) => {
                              e.stopPropagation();
                              onRunNode(node.id);
                            }}
                            className="text-muted-foreground hover:text-foreground transition-colors p-1 rounded hover:bg-muted"
                            title="Run step"
                          >
                            <Play className="h-3 w-3" />
                          </button>
                        )}
                      </div>

                      <div className="text-xs font-medium truncate">{node.label}</div>

                      {node.run?.ref && (
                        <div className="text-[11px] text-muted-foreground font-mono bg-muted/40 px-2 py-1 rounded truncate">
                          {node.run.ref}
                        </div>
                      )}
                    </div>

                    {(incomingEdges.length > 0 || outgoingEdges.length > 0) && (
                      <div className="flex items-center justify-between pt-3 mt-2 border-t border-border/40 text-[10px] text-muted-foreground">
                        <span>
                          {incomingEdges.length > 0 && `deps: ${incomingEdges.length}`}
                        </span>
                        <span>
                          {outgoingEdges.length > 0 && `leads to: ${outgoingEdges.length}`}
                        </span>
                      </div>
                    )}
                  </div>
                );
              })}
            </div>
          </div>
        ))}
      </div>
    </div>
  );
}
