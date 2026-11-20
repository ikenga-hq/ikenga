import { useEffect, useState } from 'react';
import { AlertCircle, Loader2, RotateCcw, ZoomIn, ZoomOut } from 'lucide-react';
import { Button } from '@/components/ui/button';
import { fsRead } from '@/lib/tauri-cmd';
import { CannotPreviewPlate } from '../chrome/cannot-preview-plate';
import { isPreviewTooLarge } from '../lib/preview-cap';

interface ImageViewProps {
	path: string;
}

export function ImageView({ path }: ImageViewProps) {
	const [state, setState] = useState<
		| { kind: 'loading' }
		| { kind: 'ready'; url: string; mime: string }
		| { kind: 'error'; message: string }
	>({ kind: 'loading' });
	const [scale, setScale] = useState(1);
	const [failed, setFailed] = useState(false);

	useEffect(() => {
		let cancelled = false;
		let blobUrl: string | null = null;
		setState({ kind: 'loading' });
		setScale(1);
		setFailed(false);

		fsRead(path)
			.then((res) => {
				if (cancelled) return;
				const blob = new Blob([new Uint8Array(res.bytes)], {
					type: res.mime || 'image/*',
				});
				blobUrl = URL.createObjectURL(blob);
				setState({ kind: 'ready', url: blobUrl, mime: res.mime });
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
			if (blobUrl) URL.revokeObjectURL(blobUrl);
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
		if (isPreviewTooLarge(state.message)) {
			return <CannotPreviewPlate path={path} message={state.message} />;
		}
		return (
			<div className="flex h-full items-start justify-center p-6 text-xs text-destructive">
				<AlertCircle className="mr-2 mt-0.5 h-4 w-4 shrink-0" />
				<span className="break-all">{state.message}</span>
			</div>
		);
	}

	if (failed) {
		return (
			<CannotPreviewPlate
				path={path}
				reason="This image couldn't be displayed. The file may be damaged, or use a format this viewer can't draw."
			/>
		);
	}

	return (
		<div className="flex h-full flex-col">
			<div className="flex shrink-0 items-center gap-2 border-b border-border bg-muted/20 px-3 py-1.5 text-xs">
				<Button
					variant="ghost"
					size="sm"
					className="h-7 w-7 p-0"
					onClick={() => setScale((s) => Math.max(0.1, s - 0.25))}
				>
					<ZoomOut className="h-3.5 w-3.5" />
				</Button>
				<span className="tabular-nums text-muted-foreground">{Math.round(scale * 100)}%</span>
				<Button
					variant="ghost"
					size="sm"
					className="h-7 w-7 p-0"
					onClick={() => setScale((s) => Math.min(8, s + 0.25))}
				>
					<ZoomIn className="h-3.5 w-3.5" />
				</Button>
				<Button
					variant="ghost"
					size="sm"
					className="h-7 w-7 p-0"
					onClick={() => setScale(1)}
					title="Reset zoom"
				>
					<RotateCcw className="h-3.5 w-3.5" />
				</Button>
				<span className="ml-auto font-mono text-[10px] text-muted-foreground">{state.mime}</span>
			</div>
			<div className="flex-1 overflow-auto bg-[radial-gradient(circle,_var(--muted)_1px,_transparent_1px)] [background-size:16px_16px]">
				<div className="flex min-h-full min-w-full items-center justify-center p-6">
					<img
						src={state.url}
						alt={path}
						onError={() => setFailed(true)}
						style={{ transform: `scale(${scale})`, transformOrigin: 'center' }}
						className="max-w-none transition-transform duration-100"
					/>
				</div>
			</div>
		</div>
	);
}
