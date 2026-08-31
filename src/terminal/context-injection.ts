import { listen } from '@/lib/tauri-cmd';

export interface ContextInjectionPayload {
	cwd?: string;
	openArtifact?: string;
	selection?: string;
	workspaceName?: string;
}

export function buildAdditionalContext(payload: ContextInjectionPayload): string {
	const parts: string[] = [];

	if (payload.workspaceName) {
		parts.push(`Workspace: ${payload.workspaceName}`);
	}
	if (payload.cwd) {
		parts.push(`CWD: ${payload.cwd}`);
	}
	if (payload.openArtifact) {
		parts.push(`Open Artifact: ${payload.openArtifact}`);
	}
	if (payload.selection) {
		parts.push(`Selection:\n${payload.selection}`);
	}

	return parts.join('\n\n');
}

export function initContextInjection(
	getContext: () => ContextInjectionPayload,
	onInject: (additionalContext: string) => void
) {
	let unlisten: (() => void) | undefined;

	listen<{ hook_event_name?: string; session_id?: string }>('hooks://event', (event) => {
		const p = event.payload;
		if (!p) return;

		if (p.hook_event_name === 'UserPromptSubmit' || p.hook_event_name === 'SessionStart') {
			const ctxData = getContext();
			const additionalContext = buildAdditionalContext(ctxData);
			if (additionalContext) {
				console.log(`[ContextInjection] Injecting additionalContext for session ${p.session_id}`);
				onInject(additionalContext);
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
