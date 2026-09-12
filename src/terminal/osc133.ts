/**
 * OSC 133 Semantic Prompt Integration (WP-08 / T-10)
 *
 * Parses OSC 133 escape sequences conforming to the FinalTerm / FTCS specification:
 *   - OSC 133 ; A ST           - Prompt start: register prompt boundary marker
 *   - OSC 133 ; B ST           - Command start: prompt finished, input begins
 *   - OSC 133 ; C ST           - Command executed / output start
 *   - OSC 133 ; D [; <code>] ST - Command finished: surfaces exit code gutter decoration
 *   - OSC 133 ; P ; Cwd=<path>  - Working directory update
 *
 * Provides prompt jump navigation (Cmd+Up / Cmd+Down) between prompt boundaries.
 */

import type { IDisposable, IDecoration, IMarker, Terminal } from '@xterm/xterm';

export interface SemanticPromptRecord {
	id: number;
	marker: IMarker;
	state: 'prompt' | 'input' | 'running' | 'finished';
	exitCode?: number;
	decoration?: IDecoration;
	timestamp: number;
}

export interface SemanticPromptsOptions {
	onCwd?: (cwd: string) => void;
}

export interface SemanticPromptsManager extends IDisposable {
	/** Jump to previous prompt (Cmd+Up / Ctrl+Up) */
	jumpToPrevPrompt(): boolean;
	/** Jump to next prompt (Cmd+Down / Ctrl+Down) */
	jumpToNextPrompt(): boolean;
	/** Get all tracked active prompts */
	getPrompts(): readonly SemanticPromptRecord[];
	/** Parse raw OSC 133 data directly */
	handleOsc133(data: string): boolean;
}

/**
 * Render subtle exit code gutter marker next to completed command.
 */
export function renderGutterMarker(element: HTMLElement, exitCode: number): void {
	element.innerHTML = '';
	element.setAttribute('data-testid', 'terminal-gutter-marker');
	element.setAttribute('data-exit-code', String(exitCode));
	element.className = `terminal-gutter-marker ${exitCode === 0 ? 'success' : 'failure'}`;
	element.title = exitCode === 0 ? 'Command succeeded (0)' : `Command failed (${exitCode})`;
	element.style.pointerEvents = 'auto';
	element.style.display = 'flex';
	element.style.alignItems = 'center';
	element.style.justifyContent = 'center';
	element.style.overflow = 'visible';
	element.style.zIndex = '5';

	const dot = document.createElement('span');
	dot.className = `terminal-gutter-dot ${exitCode === 0 ? 'terminal-gutter-dot-success' : 'terminal-gutter-dot-failure'}`;
	dot.style.display = 'inline-block';
	dot.style.width = '6px';
	dot.style.height = '6px';
	dot.style.borderRadius = '50%';
	dot.style.transform = 'translateX(-10px)';
	dot.style.backgroundColor = exitCode === 0 ? 'rgba(34, 197, 94, 0.75)' : 'rgba(239, 68, 68, 0.85)';
	element.appendChild(dot);
}

export function jumpToPrevPrompt(term: Terminal, prompts: readonly SemanticPromptRecord[]): boolean {
	const currentLine = term.buffer.active.viewportY;
	const candidates = prompts
		.filter((p) => !p.marker.isDisposed && p.marker.line >= 0 && p.marker.line < currentLine)
		.sort((a, b) => b.marker.line - a.marker.line);

	if (candidates.length > 0) {
		term.scrollToLine(candidates[0].marker.line);
		return true;
	}
	if (currentLine > 0) {
		term.scrollToTop();
		return true;
	}
	return false;
}

export function jumpToNextPrompt(term: Terminal, prompts: readonly SemanticPromptRecord[]): boolean {
	const currentLine = term.buffer.active.viewportY;
	const candidates = prompts
		.filter((p) => !p.marker.isDisposed && p.marker.line >= 0 && p.marker.line > currentLine)
		.sort((a, b) => a.marker.line - b.marker.line);

	if (candidates.length > 0) {
		term.scrollToLine(candidates[0].marker.line);
		return true;
	}
	term.scrollToBottom();
	return true;
}

/**
 * Installs the OSC 133 semantic prompt handler and lifecycle management on an xterm instance.
 */
export function setupSemanticPrompts(
	term: Terminal,
	options?: SemanticPromptsOptions
): SemanticPromptsManager {
	const prompts: SemanticPromptRecord[] = [];
	let activePrompt: SemanticPromptRecord | null = null;

	const handleOsc133 = (data: string): boolean => {
		if (!data) return true;
		const type = data.charAt(0);

		if (type === 'A') {
			// OSC 133;A: Prompt start
			const marker = term.registerMarker(0);
			if (marker) {
				const record: SemanticPromptRecord = {
					id: marker.id,
					marker,
					state: 'prompt',
					timestamp: Date.now(),
				};
				prompts.push(record);
				activePrompt = record;

				marker.onDispose(() => {
					const idx = prompts.findIndex((p) => p.id === marker.id);
					if (idx !== -1) {
						prompts.splice(idx, 1);
					}
					if (activePrompt?.id === marker.id) {
						activePrompt = null;
					}
				});
			}
			return true;
		}

		if (type === 'B') {
			// OSC 133;B: Command input start
			if (activePrompt) {
				activePrompt.state = 'input';
			}
			return true;
		}

		if (type === 'C') {
			// OSC 133;C: Output start (command executing)
			if (activePrompt) {
				activePrompt.state = 'running';
			}
			return true;
		}

		if (type === 'D') {
			// OSC 133;D [; <code>]: Command finished
			let exitCode = 0;
			const parts = data.split(';');
			if (parts.length > 1) {
				const parsed = parseInt(parts[1], 10);
				if (!Number.isNaN(parsed)) {
					exitCode = parsed;
				}
			}

			if (activePrompt) {
				activePrompt.state = 'finished';
				activePrompt.exitCode = exitCode;

				if (!activePrompt.decoration && !activePrompt.marker.isDisposed) {
					try {
						const decoration = term.registerDecoration({
							marker: activePrompt.marker,
							anchor: 'left',
							x: 0,
							width: 1,
						});
						if (decoration) {
							activePrompt.decoration = decoration;
							decoration.onRender((element) => {
								renderGutterMarker(element, exitCode);
							});
						}
					} catch {
						// Ignored if alt-buffer or proposed API inactive
					}
				}
			}
			return true;
		}

		if (type === 'P') {
			// OSC 133;P;Cwd=<path>
			const parts = data.split(';');
			if (parts[1]?.startsWith('Cwd=')) {
				const cwdVal = parts[1].slice(4).trim();
				if (cwdVal) {
					options?.onCwd?.(cwdVal);
				}
			}
			return true;
		}

		return true;
	};

	const oscDisposable = term.parser.registerOscHandler(133, handleOsc133);

	return {
		jumpToPrevPrompt: () => jumpToPrevPrompt(term, prompts),
		jumpToNextPrompt: () => jumpToNextPrompt(term, prompts),
		getPrompts: () => prompts,
		handleOsc133,
		dispose: () => {
			try {
				oscDisposable.dispose();
			} catch {
				// Ignore disposal errors
			}
			for (const p of prompts) {
				try {
					p.decoration?.dispose();
				} catch {
					// Ignore disposal errors
				}
				try {
					p.marker.dispose();
				} catch {
					// Ignore disposal errors
				}
			}
			prompts.length = 0;
			activePrompt = null;
		},
	};
}
