// ikenga:auto Fence Parser & Visual Boundary Decorator (WP-25)
//
// Fences mark regions of files that autonomous AI agents (Chi) may safely update
// without clobbering surrounding human edits.
//
// Grammar:
//   <!-- ikenga:auto:start [id] -->
//   ... body ...
//   <!-- ikenga:auto:end [id] -->
//
// Also supports single-marker and legacy fences:
//   <!-- ikenga:auto:start --> ... <!-- ikenga:auto:end -->
//   <!-- ikenga:auto [id] -->

export interface AutoFence {
	id: string;
	startTag: string;
	endTag: string;
	startIndex: number;
	endIndex: number;
	startLine: number;
	endLine: number;
	bodyStartIndex: number;
	bodyEndIndex: number;
	body: string;
}

export interface FenceLineDecoration {
	lineNumber: number; // 1-indexed
	type: 'start-fence' | 'inside-fence' | 'end-fence' | 'outside';
	fenceId?: string;
	badge?: string;
	className: string;
}

const START_REGEX = /<!--\s*ikenga:auto:start(?:\s+([\w-]+))?\s*-->/g;
const END_REGEX = /<!--\s*ikenga:auto:end(?:\s+([\w-]+))?\s*-->/g;

/**
 * Parses all `<!-- ikenga:auto:start --> ... <!-- ikenga:auto:end -->` blocks in `text`.
 */
export function parseAutoFences(text: string): AutoFence[] {
	const fences: AutoFence[] = [];
	const lines = text.split(/\r?\n/);

	// Compute line offsets for accurate line number mapping
	const lineOffsets: number[] = [0];
	let currentOffset = 0;
	for (const line of lines) {
		currentOffset += line.length + 1; // +1 for newline approximation
		lineOffsets.push(currentOffset);
	}

	function getLineNumber(charIndex: number): number {
		let line = 1;
		let cursor = 0;
		for (let i = 0; i < lines.length; i++) {
			const lineLen = lines[i].length + 1;
			if (cursor + lineLen > charIndex) {
				return i + 1;
			}
			cursor += lineLen;
			line = i + 1;
		}
		return line;
	}

	START_REGEX.lastIndex = 0;
	let startMatch: RegExpExecArray | null;

	while ((startMatch = START_REGEX.exec(text)) !== null) {
		const startTag = startMatch[0];
		const fenceId = startMatch[1] || 'body';
		const startIndex = startMatch.index;
		const bodyStartIndex = startIndex + startTag.length;

		// Find matching end tag
		END_REGEX.lastIndex = bodyStartIndex;
		let endMatch: RegExpExecArray | null = null;

		while ((endMatch = END_REGEX.exec(text)) !== null) {
			const endId = endMatch[1] || 'body';
			// Match either same id or default body
			if (endId === fenceId || !startMatch[1]) {
				break;
			}
		}

		if (endMatch) {
			const endTag = endMatch[0];
			const bodyEndIndex = endMatch.index;
			const endIndex = bodyEndIndex + endTag.length;
			const body = text.slice(bodyStartIndex, bodyEndIndex);

			fences.push({
				id: fenceId,
				startTag,
				endTag,
				startIndex,
				endIndex,
				startLine: getLineNumber(startIndex),
				endLine: getLineNumber(endIndex),
				bodyStartIndex,
				bodyEndIndex,
				body,
			});

			// Advance START_REGEX past this end tag to avoid overlap
			START_REGEX.lastIndex = endIndex;
		}
	}

	return fences;
}

/**
 * Replaces the body of a specific fence while preserving outer contents and other fences.
 * If the fence does not exist, returns original text.
 */
export function replaceFenceBody(text: string, fenceId: string, newBody: string): string {
	const fences = parseAutoFences(text);
	const target = fences.find((f) => f.id === fenceId) ?? fences[0];

	if (!target) {
		return text;
	}

	// Ensure clean newlines around replacement
	const trimmedBody = newBody.startsWith('\n') ? newBody : `\n${newBody}`;
	const formattedBody = trimmedBody.endsWith('\n') ? trimmedBody : `${trimmedBody}\n`;

	return (
		text.slice(0, target.bodyStartIndex) +
		formattedBody +
		text.slice(target.bodyEndIndex)
	);
}

/**
 * Wraps content in ikenga:auto boundary fences.
 */
export function wrapInFence(content: string, fenceId = 'body'): string {
	const clean = content.trim();
	return `<!-- ikenga:auto:start ${fenceId} -->\n${clean}\n<!-- ikenga:auto:end ${fenceId} -->\n`;
}

/**
 * Computes per-line visual decorations for code editors.
 */
export function getFenceLineDecorations(text: string): FenceLineDecoration[] {
	const lines = text.split(/\r?\n/);
	const fences = parseAutoFences(text);
	const decorations: FenceLineDecoration[] = [];

	for (let i = 0; i < lines.length; i++) {
		const lineNum = i + 1;
		const matchingFence = fences.find(
			(f) => lineNum >= f.startLine && lineNum <= f.endLine
		);

		if (!matchingFence) {
			decorations.push({
				lineNumber: lineNum,
				type: 'outside',
				className: 'ikenga-line-user',
			});
		} else if (lineNum === matchingFence.startLine) {
			decorations.push({
				lineNumber: lineNum,
				type: 'start-fence',
				fenceId: matchingFence.id,
				badge: `Chi auto-zone: ${matchingFence.id}`,
				className: 'ikenga-fence-boundary ikenga-fence-start',
			});
		} else if (lineNum === matchingFence.endLine) {
			decorations.push({
				lineNumber: lineNum,
				type: 'end-fence',
				fenceId: matchingFence.id,
				badge: `End auto-zone: ${matchingFence.id}`,
				className: 'ikenga-fence-boundary ikenga-fence-end',
			});
		} else {
			decorations.push({
				lineNumber: lineNum,
				type: 'inside-fence',
				fenceId: matchingFence.id,
				className: 'ikenga-fence-inside',
			});
		}
	}

	return decorations;
}
