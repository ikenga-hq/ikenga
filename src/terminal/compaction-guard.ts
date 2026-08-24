import { listen } from '@tauri-apps/api/event';

export interface CompactionGuardOptions {
	onPreCompact?: (sessionId: string) => void;
	onHtmlWrite?: (filePath: string) => void;
}

export function initCompactionGuard(options: CompactionGuardOptions = {}) {
	let unlisten: (() => void) | undefined;

	listen<{
		hook_event_name?: string;
		session_id?: string;
		tool_name?: string;
		tool_input?: { path?: string; target_file?: string };
	}>('hooks://event', (event) => {
		const payload = event.payload;
		if (!payload) return;

		// WP-11: Handle PreCompact event
		if (payload.hook_event_name === 'PreCompact' && payload.session_id) {
			console.log(`[CompactionGuard] PreCompact triggered for session ${payload.session_id}`);
			if (options.onPreCompact) {
				options.onPreCompact(payload.session_id);
			}
		}

		// WP-11: Choreography trigger for HTML/visual writes
		if (payload.hook_event_name === 'PostToolUse') {
			const filePath = payload.tool_input?.path || payload.tool_input?.target_file;
			if (filePath && (filePath.endsWith('.html') || filePath.endsWith('.excalidraw'))) {
				console.log(`[CompactionGuard] Visual asset modified: ${filePath}`);
				// User directive: do NOT auto-open pane, emit notification hint instead
				if (options.onHtmlWrite) {
					options.onHtmlWrite(filePath);
				}
			}
		}
	})
		.then((fn) => {
			unlisten = fn;
		})
		.catch(() => {});

	return () => {
		if (unlisten) unlisten();
	};
}
