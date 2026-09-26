// D-06 shared part (WP-57): "the JSON it will write" preview every editing
// surface shows (product principle 3 — design-spec-D-03-07.md §Shared
// rules). Read-only, mono, with the object's own keys lightly highlighted —
// display only (G-ACTIONS §11 item 7: the file's real serialization is
// whatever `saveUserAction` / `writeActionsFile` produce, not this).

import { cn } from '@/components/ui/utils';

export interface JsonPreviewProps {
	value: unknown;
	className?: string;
	/** A short mono caption above the block, e.g. the file path it belongs to. */
	caption?: string;
}

const KEY_RE = /^(\s*)"([^"]+)":(\s?)(.*)$/;

function renderLine(line: string, i: number) {
	const m = line.match(KEY_RE);
	// biome-ignore lint/suspicious/noArrayIndexKey: a JSON.stringify dump's lines never reorder
	if (!m) return <div key={i}>{line}</div>;
	const [, indent, key, sep, rest] = m;
	return (
		// biome-ignore lint/suspicious/noArrayIndexKey: a JSON.stringify dump's lines never reorder
		<div key={i}>
			{indent}
			<span className="jp-key">"{key}"</span>:{sep}
			<span className="jp-val">{rest}</span>
		</div>
	);
}

export function JsonPreview({ value, className, caption }: JsonPreviewProps) {
	const text = value === undefined ? 'undefined' : JSON.stringify(value, null, 2);
	return (
		<div className={cn('json-preview', className)}>
			{caption && <div className="jp-caption">{caption}</div>}
			<pre className="fm">{text.split('\n').map(renderLine)}</pre>
		</div>
	);
}
