import { describe, expect, it } from 'vitest';
import {
	parseAutoFences,
	replaceFenceBody,
	wrapInFence,
	getFenceLineDecorations,
} from './fence-decorator';

describe('fence-decorator (WP-25)', () => {
	const sampleMarkdown = `---
name: release-notes
description: Generates release notes
allowed-tools: Read, Grep
---

# Release Notes Generator

Here is human introduction text that must never be clobbered.

<!-- ikenga:auto:start body -->
- Fixed bug in auth token refresh
- Added new create surface
<!-- ikenga:auto:end body -->

Human footer notes here.
`;

	it('parses auto fences with id, startLine, and endLine', () => {
		const fences = parseAutoFences(sampleMarkdown);

		expect(fences.length).toBe(1);
		expect(fences[0].id).toBe('body');
		expect(fences[0].startTag).toBe('<!-- ikenga:auto:start body -->');
		expect(fences[0].endTag).toBe('<!-- ikenga:auto:end body -->');
		expect(fences[0].body).toContain('Fixed bug in auth token refresh');
		expect(fences[0].startLine).toBeGreaterThan(5);
		expect(fences[0].endLine).toBeGreaterThan(fences[0].startLine);
	});

	it('parses multiple fences with different ids', () => {
		const multiFenceMd = `# Multi Fences

<!-- ikenga:auto:start header -->
Header zone
<!-- ikenga:auto:end header -->

Manual middle content

<!-- ikenga:auto:start tasks -->
Task zone
<!-- ikenga:auto:end tasks -->
`;
		const fences = parseAutoFences(multiFenceMd);
		expect(fences.length).toBe(2);
		expect(fences[0].id).toBe('header');
		expect(fences[1].id).toBe('tasks');
	});

	it('replaces only the fenced body while preserving outer content', () => {
		const newContent = 'Updated release notes from Chi';
		const updated = replaceFenceBody(sampleMarkdown, 'body', newContent);

		// Outer content intact
		expect(updated).toContain('name: release-notes');
		expect(updated).toContain('Here is human introduction text that must never be clobbered.');
		expect(updated).toContain('Human footer notes here.');

		// Boundary tags intact
		expect(updated).toContain('<!-- ikenga:auto:start body -->');
		expect(updated).toContain('<!-- ikenga:auto:end body -->');

		// New body placed inside
		expect(updated).toContain(newContent);
		expect(updated).not.toContain('Fixed bug in auth token refresh');
	});

	it('wraps content in fence tags correctly', () => {
		const raw = 'Content to protect';
		const wrapped = wrapInFence(raw, 'custom-zone');

		expect(wrapped).toBe(
			'<!-- ikenga:auto:start custom-zone -->\nContent to protect\n<!-- ikenga:auto:end custom-zone -->\n'
		);
	});

	it('computes line-by-line decorations', () => {
		const decorations = getFenceLineDecorations(sampleMarkdown);
		expect(decorations.length).toBeGreaterThan(10);

		const startDec = decorations.find((d) => d.type === 'start-fence');
		expect(startDec).toBeDefined();
		expect(startDec?.fenceId).toBe('body');
		expect(startDec?.className).toContain('ikenga-fence-start');

		const insideDec = decorations.find((d) => d.type === 'inside-fence');
		expect(insideDec).toBeDefined();
		expect(insideDec?.className).toBe('ikenga-fence-inside');

		const endDec = decorations.find((d) => d.type === 'end-fence');
		expect(endDec).toBeDefined();
		expect(endDec?.className).toContain('ikenga-fence-end');

		const outsideDec = decorations.find((d) => d.type === 'outside');
		expect(outsideDec).toBeDefined();
		expect(outsideDec?.className).toBe('ikenga-line-user');
	});
});
