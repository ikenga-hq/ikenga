// "Run <engine> login" from the target picker's signed-out state.

import { usePaneStore } from '@/lib/panes/pane-store';
import { AGENT_WSL_DISTRO_KEY } from '@/lib/shell-profiles';
import { type DetectedAgent, settingsGet } from '@/lib/tauri-cmd';
import { buildLoginCmd } from '@/terminal/claude-wrap';
import { createTerminalSession } from '@/terminal/single-terminal';

/** Open `<engine> login` in a new terminal tab. A WSL-only CLI (detected as
 *  the display string `"/usr/bin/claude (WSL)"`, not a path) logs in inside
 *  the configured distro (`buildLoginCmd`); an unreadable distro setting
 *  falls back to the default distro, as the backend's probes do. */
export async function openLoginTerminal(engine: Pick<DetectedAgent, 'id' | 'executable_path'>): Promise<void> {
	let distro: string | null = null;
	if (engine.executable_path.endsWith(' (WSL)')) {
		try {
			distro = await settingsGet(AGENT_WSL_DISTRO_KEY);
		} catch (err) {
			console.warn('[target-picker] reading the WSL distro setting failed; using the default distro', err);
		}
	}
	const sessionId = createTerminalSession({
		cmd: buildLoginCmd(engine.executable_path, distro),
		title: `${engine.id} login`,
	});
	const panes = usePaneStore.getState();
	panes.placeView(panes.focusedId, { kind: 'terminal', sessionId }, 'append');
}
