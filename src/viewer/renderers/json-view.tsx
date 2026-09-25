// D-08 `renderers` — the JSON collapsible-tree renderer
// (designs/pane-chrome.html `RENDERERS` id absent today, added by WP-44 per
// the D-08 brief's "JSON (tree, collapsible)" cell). Distinct from
// `pen-view.tsx`'s split tree+detail (built for Pencil's frame-oriented
// documents) — this is a single inline nested tree, closer to the `.rjson`
// mock: every object/array row expands in place, primitives render on the
// same line as their key.

import { useEffect, useState } from 'react';
import { AlertCircle, ChevronDown, ChevronRight } from 'lucide-react';
import { ErrorState, LoadingState } from '@/components/states';
import { fsRead } from '@/lib/tauri-cmd';

interface JsonViewProps {
	path: string;
}

type LoadState =
	| { kind: 'loading' }
	| { kind: 'ready'; value: unknown }
	| { kind: 'error'; message: string };

export function JsonView({ path }: JsonViewProps) {
	const [state, setState] = useState<LoadState>({ kind: 'loading' });

	useEffect(() => {
		let cancelled = false;
		setState({ kind: 'loading' });
		fsRead(path)
			.then((res) => {
				if (cancelled) return;
				const text = new TextDecoder('utf-8', { fatal: false }).decode(new Uint8Array(res.bytes));
				try {
					setState({ kind: 'ready', value: JSON.parse(text) });
				} catch (err) {
					setState({ kind: 'error', message: err instanceof Error ? err.message : String(err) });
				}
			})
			.catch((err) => {
				if (cancelled) return;
				setState({ kind: 'error', message: err instanceof Error ? err.message : String(err) });
			});
		return () => {
			cancelled = true;
		};
	}, [path]);

	if (state.kind === 'loading') {
		return <LoadingState data-state="loading" fill heading="Loading…" />;
	}
	if (state.kind === 'error') {
		return (
			<ErrorState
				data-state="error"
				fill
				icon={AlertCircle}
				heading="Couldn't parse this file as JSON"
				body={<span className="break-all">{state.message}</span>}
			/>
		);
	}

	return (
		<div className="h-full overflow-auto px-2 py-2 font-mono text-xs">
			<JsonNode label={null} value={state.value} depth={0} />
		</div>
	);
}

const PUNCT = 'text-muted-foreground';
const KEY_COLOR = 'text-sky-600 dark:text-sky-400';
const STRING_COLOR = 'text-emerald-600 dark:text-emerald-400';
const NUMBER_COLOR = 'text-amber-600 dark:text-amber-400';
const BOOL_COLOR = 'text-purple-600 dark:text-purple-400';

function JsonNode({
	label,
	value,
	depth,
}: {
	label: string | null;
	value: unknown;
	depth: number;
}) {
	const isArr = Array.isArray(value);
	const isObj = !isArr && value !== null && typeof value === 'object';
	const [open, setOpen] = useState(depth < 1);

	if (!isArr && !isObj) {
		return (
			<div className="flex items-start gap-1 py-0.5" style={{ paddingLeft: depth * 14 }}>
				<span className="w-3 shrink-0" />
				{label !== null && (
					<>
						<span className={KEY_COLOR}>"{label}"</span>
						<span className={PUNCT}>:</span>
					</>
				)}
				<Leaf value={value} />
			</div>
		);
	}

	const entries: Array<[string, unknown]> = isArr
		? (value as unknown[]).map((v, i) => [String(i), v])
		: Object.entries(value as Record<string, unknown>);

	return (
		<div>
			<button
				type="button"
				onClick={() => setOpen((o) => !o)}
				className="flex w-full items-start gap-1 rounded py-0.5 text-left hover:bg-muted/50"
				style={{ paddingLeft: depth * 14 }}
				aria-expanded={open}
			>
				{open ? (
					<ChevronDown className="mt-0.5 h-3 w-3 shrink-0" />
				) : (
					<ChevronRight className="mt-0.5 h-3 w-3 shrink-0" />
				)}
				{label !== null && (
					<>
						<span className={KEY_COLOR}>"{label}"</span>
						<span className={PUNCT}>:</span>
					</>
				)}
				<span className={PUNCT}>
					{isArr ? `[${entries.length}]` : `{${entries.length}}`}
				</span>
			</button>
			{open &&
				entries.map(([k, v]) => <JsonNode key={k} label={k} value={v} depth={depth + 1} />)}
		</div>
	);
}

function Leaf({ value }: { value: unknown }) {
	if (value === null) return <span className={BOOL_COLOR}>null</span>;
	if (typeof value === 'string') return <span className={STRING_COLOR}>"{value}"</span>;
	if (typeof value === 'number') return <span className={NUMBER_COLOR}>{value}</span>;
	if (typeof value === 'boolean') return <span className={BOOL_COLOR}>{String(value)}</span>;
	return <span>{String(value)}</span>;
}
