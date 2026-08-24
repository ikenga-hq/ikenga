// Brand logo for an onboarding engine row.
//
// Maps the onboarding's engine ids (claude-code, codex, gemini,
// cursor-agent, ollama) to `@lobehub/icons` color variants. Unknown ids
// fall back to a question-mark glyph so we don't crash on a future
// engine that hasn't been wired yet.

import { HelpCircle, Terminal } from 'lucide-react';
import Claude from '@lobehub/icons/es/Claude';
import Codex from '@lobehub/icons/es/Codex';
import Cursor from '@lobehub/icons/es/Cursor';
import Gemini from '@lobehub/icons/es/Gemini';
import Ollama from '@lobehub/icons/es/Ollama';

import { cn } from '@/components/ui/utils';

export type EngineId =
	| 'claude-code'
	| 'codex'
	| 'gemini'
	| 'cursor-agent'
	| 'opencode'
	| 'pi'
	| 'ollama'
	| (string & {});

export function EngineLogo({
	engineId,
	size,
	className,
}: {
	engineId: EngineId;
	size?: number;
	className?: string;
}) {
	const px = size ?? 22;
	// Use the `.Avatar` variant uniformly — Cursor and Ollama only ship that
	// shape, and using Avatar everywhere keeps the onboarding cards visually
	// consistent (all show as filled square brand tiles in the 9×9 slot).
	switch (engineId) {
		case 'claude-code':
			return <Claude.Avatar size={px} className={className} />;
		case 'codex':
			return <Codex.Avatar size={px} className={className} />;
		case 'gemini':
			return <Gemini.Avatar size={px} className={className} />;
		case 'cursor-agent':
			return <Cursor.Avatar size={px} className={className} />;
		case 'opencode':
			return (
				<div
					className={cn(
						'flex items-center justify-center rounded bg-emerald-600/20 text-emerald-400 font-mono font-semibold',
						className
					)}
					style={{ width: px, height: px }}
				>
					<Terminal size={Math.round(px * 0.7)} />
				</div>
			);
		case 'pi':
			return (
				<div
					className={cn(
						'flex items-center justify-center rounded bg-amber-600/20 text-amber-400 font-serif font-bold leading-none select-none',
						className
					)}
					style={{ width: px, height: px, fontSize: Math.round(px * 0.75) }}
				>
					π
				</div>
			);
		case 'ollama':
			return <Ollama.Avatar size={px} className={className} />;
		default:
			return <HelpCircle width={px} height={px} className={className} />;
	}
}
