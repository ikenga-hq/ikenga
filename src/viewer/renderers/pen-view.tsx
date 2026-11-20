import { useEffect, useMemo, useState } from 'react';
import { AlertCircle, ChevronDown, ChevronRight, Loader2 } from 'lucide-react';
import { fsRead } from '@/lib/tauri-cmd';
import { PreviewTooLargePlate } from '../chrome/preview-too-large-plate';
import { isPreviewTooLarge } from '../lib/preview-cap';

interface PenViewProps {
	path: string;
}

// Pencil (.pen) files are JSON. v1 renders a tree on the left (frames →
// sections → elements) and the JSON of the selected node on the right. A
// rasterized preview is left to a later iteration — would require either
// the Pencil MCP (unstable) or a Rust-side rasterizer.
//
// The structure varies between Pencil versions. We do a defensive walk: any
// object becomes a node, arrays become indexed children, primitive leaves
// stop the walk.
export function PenView({ path }: PenViewProps) {
	const [state, setState] = useState<
		{ kind: 'loading' } | { kind: 'ready'; doc: unknown } | { kind: 'error'; message: string }
	>({ kind: 'loading' });
	const [selected, setSelected] = useState<string[]>([]);

	useEffect(() => {
		let cancelled = false;
		setState({ kind: 'loading' });
		fsRead(path)
			.then((res) => {
				if (cancelled) return;
				try {
					const text = new TextDecoder('utf-8', { fatal: false }).decode(new Uint8Array(res.bytes));
					setState({ kind: 'ready', doc: JSON.parse(text) });
				} catch (err) {
					setState({
						kind: 'error',
						message: err instanceof Error ? err.message : String(err),
					});
				}
			})
			.catch((err) => {
				if (cancelled) return;
				setState({
					kind: 'error',
					message: err instanceof Error ? err.message : String(err),
				});
			});
		return () => {
			cancelled = true;
		};
	}, [path]);

	const selectedValue = useMemo(() => {
		if (state.kind !== 'ready') return undefined;
		return getAt(state.doc, selected);
	}, [state, selected]);

	if (state.kind === 'loading') {
		return (
			<div className="flex h-full items-center justify-center text-xs text-muted-foreground">
				<Loader2 className="mr-2 h-4 w-4 animate-spin" /> Loading…
			</div>
		);
	}
	if (state.kind === 'error') {
		if (isPreviewTooLarge(state.message)) {
			return <PreviewTooLargePlate path={path} message={state.message} />;
		}
		return (
			<div className="flex h-full items-start justify-center p-6 text-xs text-destructive">
				<AlertCircle className="mr-2 mt-0.5 h-4 w-4 shrink-0" />
				<span className="break-all">{state.message}</span>
			</div>
		);
	}

	return (
		<div className="grid h-full grid-cols-[280px_1fr]">
			<div className="overflow-auto border-r border-border bg-muted/10 p-2 text-xs">
				<PenNode
					label="(root)"
					value={state.doc}
					path={[]}
					selected={selected}
					onSelect={setSelected}
				/>
			</div>
			<div className="overflow-auto px-4 py-3 font-mono text-xs">
				<pre className="whitespace-pre-wrap break-words">
					{selectedValue === undefined
						? 'Select a node from the tree.'
						: safeStringify(selectedValue)}
				</pre>
			</div>
		</div>
	);
}

function PenNode({
	label,
	value,
	path,
	selected,
	onSelect,
}: {
	label: string;
	value: unknown;
	path: string[];
	selected: string[];
	onSelect: (p: string[]) => void;
}) {
	const isObj = value !== null && typeof value === 'object';
	const isArr = Array.isArray(value);
	const [open, setOpen] = useState(path.length < 2);
	const isSelected = selected.length === path.length && selected.every((s, i) => s === path[i]);

	const childEntries = useMemo<Array<[string, unknown]>>(() => {
		if (!isObj) return [];
		if (isArr) return (value as unknown[]).map((v, i) => [String(i), v]);
		return Object.entries(value as Record<string, unknown>);
	}, [value, isObj, isArr]);

	return (
		<div>
			<button
				onClick={() => {
					onSelect(path);
					if (isObj) setOpen((o) => !o);
				}}
				className={
					isSelected
						? 'flex w-full items-center gap-1 rounded bg-primary/10 px-1 py-0.5 text-left text-primary'
						: 'flex w-full items-center gap-1 rounded px-1 py-0.5 text-left hover:bg-muted/50'
				}
			>
				{isObj ? (
					open ? (
						<ChevronDown className="h-3 w-3 shrink-0" />
					) : (
						<ChevronRight className="h-3 w-3 shrink-0" />
					)
				) : (
					<span className="w-3 shrink-0" />
				)}
				<span className="truncate">
					{label}
					{isObj && (
						<span className="ml-1 text-muted-foreground">
							{isArr ? `[${childEntries.length}]` : `{${childEntries.length}}`}
						</span>
					)}
					{!isObj && <span className="ml-1 text-muted-foreground">{previewLeaf(value)}</span>}
				</span>
			</button>
			{isObj && open && (
				<div className="ml-3 border-l border-border pl-2">
					{childEntries.map(([key, child]) => (
						<PenNode
							key={key}
							label={key}
							value={child}
							path={[...path, key]}
							selected={selected}
							onSelect={onSelect}
						/>
					))}
				</div>
			)}
		</div>
	);
}

function getAt(value: unknown, path: string[]): unknown {
	let cur: unknown = value;
	for (const seg of path) {
		if (cur === null || typeof cur !== 'object') return undefined;
		if (Array.isArray(cur)) {
			cur = cur[Number(seg)];
		} else {
			cur = (cur as Record<string, unknown>)[seg];
		}
	}
	return cur;
}

function previewLeaf(v: unknown): string {
	if (typeof v === 'string') {
		const trimmed = v.length > 60 ? `${v.slice(0, 60)}…` : v;
		return `"${trimmed}"`;
	}
	return String(v);
}

function safeStringify(v: unknown): string {
	try {
		return JSON.stringify(v, null, 2);
	} catch {
		return String(v);
	}
}
