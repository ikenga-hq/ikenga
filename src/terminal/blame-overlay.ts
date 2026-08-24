export interface BlameAnnotation {
	line: number;
	author: string;
	sessionId?: string;
	turnIndex?: number;
	timestamp: string;
	commitHash?: string;
}

export interface TurnCommitBundle {
	sessionId: string;
	turnIndex: number;
	filesTouched: string[];
	message: string;
}

export function parseBlameOutput(rawBlame: string): BlameAnnotation[] {
	const lines = rawBlame.split('\n');
	const annotations: BlameAnnotation[] = [];

	let currentLine = 1;
	for (const line of lines) {
		if (!line.trim()) continue;
		const parts = line.split(/\s+/);
		const commitHash = parts[0] ? parts[0].slice(0, 8) : 'unknown';
		annotations.push({
			line: currentLine++,
			author: 'Chi / Claude',
			commitHash,
			timestamp: new Date().toISOString(),
		});
	}

	return annotations;
}

export function buildTurnCommitMessage(bundle: TurnCommitBundle): string {
	const fileList = bundle.filesTouched.map((f) => `- ${f}`).join('\n');
	return `feat(turn-${bundle.turnIndex}): ${bundle.message}\n\nSession: ${bundle.sessionId}\nFiles:\n${fileList}`;
}
