import { useEffect, useRef, useState } from 'react';
import { AlertCircle, Loader2, Music } from 'lucide-react';
import { viewerServe, viewerStop, type ViewerHandle } from '@/lib/tauri-cmd';
import { basename, dirname } from '../lib/path';

interface AudioViewProps {
	path: string;
}

// Native <audio> renderer for the artifact viewer. The waveform overlay
// (peaks.js) was dropped during the strip-down — the dep weight wasn't
// worth it for a feature that already had to gracefully degrade when
// WebAudio rejected the codec. Reachable from the file viewer the
// moment a user opens an audio file from Files mode.
export function AudioView(props: AudioViewProps) {
	return <LocalAudioView {...props} />;
}

function LocalAudioView({ path }: AudioViewProps) {
	const [state, setState] = useState<
		| { kind: 'loading' }
		| { kind: 'ready'; src: string; handle: ViewerHandle }
		| { kind: 'error'; message: string }
	>({ kind: 'loading' });
	const audioRef = useRef<HTMLAudioElement>(null);

	useEffect(() => {
		let cancelled = false;
		let handle: ViewerHandle | null = null;
		setState({ kind: 'loading' });

		viewerServe(dirname(path))
			.then((h) => {
				handle = h;
				if (cancelled) {
					void viewerStop(h.token);
					return;
				}
				setState({
					kind: 'ready',
					src: `${h.url}${basename(path)}`,
					handle: h,
				});
			})
			.catch((err) => {
				if (cancelled) return;
				setState({
					kind: 'error',
					message: err instanceof Error ? err.message : String(err),
				});
			});

		return () => {
			cancelled = true;
			if (handle) void viewerStop(handle.token);
		};
	}, [path]);

	if (state.kind === 'loading') {
		return (
			<div className="flex h-full items-center justify-center text-xs text-muted-foreground">
				<Loader2 className="mr-2 h-4 w-4 animate-spin" /> Loading…
			</div>
		);
	}
	if (state.kind === 'error') {
		return (
			<div className="flex h-full items-start justify-center p-6 text-xs text-destructive">
				<AlertCircle className="mr-2 mt-0.5 h-4 w-4 shrink-0" />
				<span className="break-all">{state.message}</span>
			</div>
		);
	}

	return (
		<div className="flex h-full flex-col bg-background">
			<div className="flex flex-1 items-center justify-center text-muted-foreground">
				<Music className="mr-2 h-8 w-8 opacity-40" />
				<span className="text-sm">{basename(path)}</span>
			</div>
			<div className="shrink-0 border-t border-border bg-muted/30 p-3">
				<audio
					ref={audioRef}
					src={state.src}
					controls
					className="w-full"
					// eslint-disable-next-line jsx-a11y/media-has-caption
				/>
			</div>
		</div>
	);
}
