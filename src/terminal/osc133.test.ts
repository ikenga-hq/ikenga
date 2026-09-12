import { describe, expect, it, vi } from 'vitest';
import { Terminal } from '@xterm/xterm';
import {
	renderGutterMarker,
	setupSemanticPrompts,
} from './osc133';

function writeAsync(term: Terminal, data: string): Promise<void> {
	return new Promise((resolve) => term.write(data, resolve));
}

describe('OSC 133 Semantic Prompts (WP-08 / T-10)', () => {
	it('parses OSC 133 escape markers cleanly without visual corruption', async () => {
		const term = new Terminal({ allowProposedApi: true });
		const manager = setupSemanticPrompts(term);

		// Emit prompt start sequence
		await writeAsync(term, '\x1b]133;A\x07');
		expect(manager.getPrompts()).toHaveLength(1);
		expect(manager.getPrompts()[0].state).toBe('prompt');

		// Emit command input sequence
		await writeAsync(term, '\x1b]133;B\x07');
		expect(manager.getPrompts()[0].state).toBe('input');

		// Emit output start sequence
		await writeAsync(term, '\x1b]133;C\x07');
		expect(manager.getPrompts()[0].state).toBe('running');

		// Emit command finished with exit code 0
		await writeAsync(term, '\x1b]133;D;0\x07');
		expect(manager.getPrompts()[0].state).toBe('finished');
		expect(manager.getPrompts()[0].exitCode).toBe(0);

		manager.dispose();
		term.dispose();
	});

	it('surfaces command exit codes as subtle gutter markers with appropriate status styling', () => {
		const container = document.createElement('div');
		renderGutterMarker(container, 0);

		expect(container.getAttribute('data-testid')).toBe('terminal-gutter-marker');
		expect(container.getAttribute('data-exit-code')).toBe('0');
		expect(container.className).toContain('success');
		expect(container.title).toContain('Command succeeded (0)');

		const dot = container.querySelector('.terminal-gutter-dot') as HTMLElement;
		expect(dot).not.toBeNull();
		expect(dot.className).toContain('terminal-gutter-dot-success');

		// Failure case
		renderGutterMarker(container, 127);
		expect(container.getAttribute('data-exit-code')).toBe('127');
		expect(container.className).toContain('failure');
		expect(container.title).toContain('Command failed (127)');

		const failDot = container.querySelector('.terminal-gutter-dot') as HTMLElement;
		expect(failDot).not.toBeNull();
		expect(failDot.className).toContain('terminal-gutter-dot-failure');
	});

	it('attaches gutter decorations upon receiving OSC 133;D command finished', async () => {
		const term = new Terminal({ allowProposedApi: true });
		const manager = setupSemanticPrompts(term);

		await writeAsync(term, '\x1b]133;A\x07');
		await writeAsync(term, 'user@host:~$ ');
		await writeAsync(term, '\x1b]133;B\x07');
		await writeAsync(term, 'false\r\n');
		await writeAsync(term, '\x1b]133;C\x07');
		await writeAsync(term, '\x1b]133;D;1\x07');

		const prompts = manager.getPrompts();
		expect(prompts).toHaveLength(1);
		expect(prompts[0].exitCode).toBe(1);
		expect(prompts[0].decoration).toBeDefined();

		manager.dispose();
		term.dispose();
	});

	it('navigates between prompt boundaries using jumpToPrevPrompt and jumpToNextPrompt', () => {
		const term = new Terminal({ allowProposedApi: true });
		const manager = setupSemanticPrompts(term);

		// Mock markers and buffer positions
		const scrollToLineSpy = vi.spyOn(term, 'scrollToLine').mockImplementation(() => {});
		const scrollToBottomSpy = vi.spyOn(term, 'scrollToBottom').mockImplementation(() => {});
		const scrollToTopSpy = vi.spyOn(term, 'scrollToTop').mockImplementation(() => {});

		// Populate 3 simulated prompts at lines 0, 15, and 30
		const prompts = manager.getPrompts() as any[];
		prompts.push(
			{
				id: 1,
				marker: { id: 1, line: 0, isDisposed: false, dispose: vi.fn(), onDispose: vi.fn() },
				state: 'finished',
			},
			{
				id: 2,
				marker: { id: 2, line: 15, isDisposed: false, dispose: vi.fn(), onDispose: vi.fn() },
				state: 'finished',
			},
			{
				id: 3,
				marker: { id: 3, line: 30, isDisposed: false, dispose: vi.fn(), onDispose: vi.fn() },
				state: 'prompt',
			}
		);

		// Current viewport is at line 30 (bottom prompt)
		Object.defineProperty(term.buffer.active, 'viewportY', { value: 30, configurable: true });

		// Jump up -> should scroll to line 15
		expect(manager.jumpToPrevPrompt()).toBe(true);
		expect(scrollToLineSpy).toHaveBeenLastCalledWith(15);

		// Viewport is now at line 15 -> Jump up -> should scroll to line 0
		Object.defineProperty(term.buffer.active, 'viewportY', { value: 15, configurable: true });
		expect(manager.jumpToPrevPrompt()).toBe(true);
		expect(scrollToLineSpy).toHaveBeenLastCalledWith(0);

		// Viewport is at line 0 -> Jump down -> should scroll to line 15
		Object.defineProperty(term.buffer.active, 'viewportY', { value: 0, configurable: true });
		expect(manager.jumpToNextPrompt()).toBe(true);
		expect(scrollToLineSpy).toHaveBeenLastCalledWith(15);

		// Viewport is at line 15 -> Jump down -> should scroll to line 30
		Object.defineProperty(term.buffer.active, 'viewportY', { value: 15, configurable: true });
		expect(manager.jumpToNextPrompt()).toBe(true);
		expect(scrollToLineSpy).toHaveBeenLastCalledWith(30);

		// Viewport is at line 30 (last prompt) -> Jump down -> should scroll to bottom
		Object.defineProperty(term.buffer.active, 'viewportY', { value: 30, configurable: true });
		expect(manager.jumpToNextPrompt()).toBe(true);
		expect(scrollToBottomSpy).toHaveBeenCalled();

		// When no prompts above and viewportY > 0 -> jump up -> scrolls to top
		prompts.length = 0;
		Object.defineProperty(term.buffer.active, 'viewportY', { value: 10, configurable: true });
		expect(manager.jumpToPrevPrompt()).toBe(true);
		expect(scrollToTopSpy).toHaveBeenCalled();

		manager.dispose();
		term.dispose();
	});

	it('handles OSC 133;P;Cwd updates through optional onCwd callback', async () => {
		const term = new Terminal({ allowProposedApi: true });
		const onCwd = vi.fn();
		const manager = setupSemanticPrompts(term, { onCwd });

		await writeAsync(term, '\x1b]133;P;Cwd=/custom/project/dir\x07');
		expect(onCwd).toHaveBeenCalledWith('/custom/project/dir');

		manager.dispose();
		term.dispose();
	});
});
