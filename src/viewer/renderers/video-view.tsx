import { useEffect, useState } from 'react';
import { AlertCircle, Loader2 } from 'lucide-react';
import { viewerServe, viewerStop, type ViewerHandle } from '@/lib/tauri-cmd';
import { basename, dirname } from '../lib/path';

interface VideoViewProps {
	path: string;
}

// Plays via the localhost viewer server so the browser can stream rather than
// loading the whole file into memory like a Blob URL would. Same security
// model as the HTML frame: token-scoped origin, allowlisted root.
export function VideoView(props: VideoViewProps) {
	return <LocalVideoView {...props} />;
}

function LocalVideoView({ path }: VideoViewProps) {
	const [state, setState] = useState<
		| { kind: 'loading' }
		| { kind: 'ready'; src: string; handle: ViewerHandle }
		| { kind: 'error'; message: string }
	>({ kind: 'loading' });

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
		<div className="flex h-full w-full items-center justify-center bg-black">
			<video
				src={state.src}
				controls
				className="max-h-full max-w-full"
				// eslint-disable-next-line jsx-a11y/media-has-caption
			/>
		</div>
	);
}
