// Capability graph · Layered DAG (Phase 4 · D-06 mode). dagre auto-ranks nodes
// by dependency depth and routes edges; an L→R / T→B flip. Nodes are kind-
// coloured cards; selection dims to the neighbourhood. d3-zoom drives pan/zoom
// on the container <g>; content renders declaratively. Consumes the shared
// filtered `view` + RendererProps.

import { useEffect, useMemo, useRef, useState } from 'react';
import dagre from 'dagre';
import { curveBasis, line as d3line } from 'd3-shape';
import { select } from 'd3-selection';
import { zoom as d3zoom, zoomIdentity, type ZoomBehavior } from 'd3-zoom';
import type { GraphEdge } from '@/lib/claude-graph';
import { cn } from '@/components/ui/utils';
import { DASHED_RELS, edgeColor, KIND_GLYPH, NGWA, type RendererProps } from './graph-shared';

const NODE_H = 30;
const pathOf = d3line<{ x: number; y: number }>()
	.curve(curveBasis)
	.x((d) => d.x)
	.y((d) => d.y);

function approxW(label: string, mono: boolean): number {
	const cw = mono ? 6.6 : 6.9;
	return Math.max(74, Math.min(190, label.length * cw + 34));
}

export function GraphDag({ graph, selected, incident, onSelect, draggedRef }: RendererProps) {
	const svgRef = useRef<SVGSVGElement>(null);
	const gRef = useRef<SVGGElement>(null);
	const zoomRef = useRef<ZoomBehavior<SVGSVGElement, unknown> | null>(null);
	const [rankdir, setRankdir] = useState<'LR' | 'TB'>('LR');

	const layout = useMemo(() => {
		const g = new dagre.graphlib.Graph({ multigraph: true });
		g.setGraph({ rankdir, nodesep: 18, ranksep: 80, edgesep: 12, marginx: 40, marginy: 40 });
		g.setDefaultEdgeLabel(() => ({}));
		for (const n of graph.nodes) {
			g.setNode(n.id, { width: approxW(n.label, n.kind === 'command'), height: NODE_H });
		}
		graph.edges.forEach((e, i) => {
			g.setEdge(e.source, e.target, {}, `e${i}`);
		});
		dagre.layout(g);
		const nodes = graph.nodes.map((n) => {
			const p = g.node(n.id);
			return { node: n, x: p.x, y: p.y, w: p.width };
		});
		const edges = graph.edges.map((e, i) => {
			const ge = g.edge(e.source, e.target, `e${i}`) as { points?: { x: number; y: number }[] };
			return { edge: e, d: ge?.points?.length ? (pathOf(ge.points) ?? '') : '' };
		});
		const gr = g.graph();
		return { nodes, edges, width: gr.width ?? 1, height: gr.height ?? 1 };
	}, [graph, rankdir]);

	// d3-zoom on the svg → transform the container <g> imperatively.
	useEffect(() => {
		const el = svgRef.current;
		const gEl = gRef.current;
		if (!el || !gEl) return;
		const zoomB = d3zoom<SVGSVGElement, unknown>()
			.scaleExtent([0.2, 4])
			.on('zoom', (ev) => {
				gEl.setAttribute('transform', ev.transform.toString());
				if (ev.sourceEvent && draggedRef) draggedRef.current = true;
			});
		zoomRef.current = zoomB;
		select(el).call(zoomB);
		return () => {
			select(el).on('.zoom', null);
		};
	}, [draggedRef]);

	// Fit the layout into view whenever it changes.
	useEffect(() => {
		const el = svgRef.current;
		const zoomB = zoomRef.current;
		if (!el || !zoomB) return;
		const W = el.clientWidth || 800;
		const H = el.clientHeight || 600;
		const pad = 40;
		const k = Math.min((W - pad * 2) / layout.width, (H - pad * 2) / layout.height, 1.1);
		const tx = (W - layout.width * k) / 2;
		const ty = (H - layout.height * k) / 2;
		(select(el) as any)
			.transition()
			.duration(380)
			.call(zoomB.transform, zoomIdentity.translate(tx, ty).scale(k));
	}, [layout]);

	const dimNode = (id: string) => incident !== null && !incident.nodes.has(id);
	const dimEdge = (e: GraphEdge) => incident !== null && !incident.edges.has(e.id);

	return (
		<>
			<svg
				ref={svgRef}
				className="ngwa-graph-svg"
				role="img"
				aria-label={`Capability graph — ${graph.nodes.length} nodes, ${graph.edges.length} links, layered DAG layout`}
				style={{ cursor: 'grab' }}
			>
				<defs>
					<marker id="dag-arr" markerWidth="6" markerHeight="6" refX="5" refY="3" orient="auto">
						<path d="M0,0L0,6L6,3z" fill="var(--border-strong)" />
					</marker>
				</defs>
				<g ref={gRef}>
					<g fill="none">
						{layout.edges.map(({ edge, d }) => (
							<path
								key={edge.id}
								d={d}
								stroke={edgeColor(edge, selected)}
								strokeWidth={incident?.edges.has(edge.id) ? 2 : 1.3}
								strokeOpacity={dimEdge(edge) ? 0.05 : selected ? 0.9 : 0.32}
								strokeDasharray={
									edge.derivation === 'heuristic' || DASHED_RELS.has(edge.rel) ? '5,4' : undefined
								}
								markerEnd="url(#dag-arr)"
							/>
						))}
					</g>
					{layout.nodes.map(({ node, x, y, w }) => {
						const isSel = selected === node.id;
						return (
							// biome-ignore lint/a11y/noStaticElementInteractions: the SVG is role="img" with a descriptive aria-label (the AT path); these <g> nodes are a sighted-mouse convenience inside that image, not an independent interactive control.
							<g
								key={node.id}
								transform={`translate(${x - w / 2},${y - NODE_H / 2})`}
								style={{ cursor: 'pointer' }}
								opacity={dimNode(node.id) ? 0.18 : 1}
								onClick={(ev) => {
									ev.stopPropagation();
									onSelect(node.id);
								}}
							>
								<rect
									width={w}
									height={NODE_H}
									rx={7}
									fill={`color-mix(in srgb, var(--nk-${node.kind}) 14%, var(--bg-raised))`}
									stroke={isSel ? NGWA : `var(--nk-${node.kind})`}
									strokeWidth={isSel ? 2.6 : 1.5}
								/>
								<text
									x={11}
									y={NODE_H / 2}
									dy="0.34em"
									className="ngwa-dag-glyph"
									fill={`var(--nk-${node.kind})`}
								>
									{KIND_GLYPH[node.kind]}
								</text>
								<text
									x={24}
									y={NODE_H / 2}
									dy="0.34em"
									className={cn('ngwa-dag-label', node.kind === 'command' && 'mono')}
									fill="var(--fg)"
								>
									{node.label}
								</text>
							</g>
						);
					})}
				</g>
			</svg>
			<div className="ngwa-dag-controls">
				<button
					type="button"
					className="ngwa-graph-pillbtn on"
					onClick={() => setRankdir((d) => (d === 'LR' ? 'TB' : 'LR'))}
					title="Flip rank direction"
				>
					{rankdir === 'LR' ? '⇉ L→R' : '⇊ T→B'}
				</button>
			</div>
		</>
	);
}
