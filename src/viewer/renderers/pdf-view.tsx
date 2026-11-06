import { useEffect, useMemo, useState } from 'react';
import {
	AlertCircle,
	ChevronLeft,
	ChevronRight,
	Download,
	Loader2,
	Search,
	ZoomIn,
	ZoomOut,
} from 'lucide-react';
import { Document, Page, pdfjs } from 'react-pdf';
// pdfjs-dist v5 has empty `exports`, so we can't import the worker as a
// bare-spec subpath. Copy is committed at public/pdf.worker.min.mjs and
// served by Vite at the root.
const workerUrl = '/pdf.worker.min.mjs';
import 'react-pdf/dist/Page/AnnotationLayer.css';
import 'react-pdf/dist/Page/TextLayer.css';
import { Button } from '@/components/ui/button';
import { fsRead } from '@/lib/tauri-cmd';
import { toast } from '@/lib/toast';
import { isBrowserHost, openLocalPath, saveBlobAs } from '@/lib/transport';
import { basename } from '../lib/path';

pdfjs.GlobalWorkerOptions.workerSrc = workerUrl;

interface PdfViewProps {
	path: string;
}

export function PdfView({ path }: PdfViewProps) {
	const [state, setState] = useState<
		{ kind: 'loading' } | { kind: 'ready'; data: Uint8Array } | { kind: 'error'; message: string }
	>({ kind: 'loading' });
	const [pageNum, setPageNum] = useState(1);
	const [numPages, setNumPages] = useState(0);
	const [scale, setScale] = useState(1.0);
	const [search, setSearch] = useState('');

	useEffect(() => {
		let cancelled = false;
		setState({ kind: 'loading' });
		setPageNum(1);
		fsRead(path)
			.then((res) => {
				if (cancelled) return;
				setState({ kind: 'ready', data: new Uint8Array(res.bytes) });
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
		};
	}, [path]);

	// react-pdf needs a stable reference for `file` — wrapping in useMemo
	// avoids re-fetching the document on every parent re-render.
	const file = useMemo(() => (state.kind === 'ready' ? { data: state.data } : null), [state]);

	if (state.kind === 'loading') {
		return (
			<div className="flex h-full items-center justify-center text-xs text-muted-foreground">
				<Loader2 className="mr-2 h-4 w-4 animate-spin" /> Loading PDF…
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
		<div className="flex h-full flex-col">
			<div className="flex shrink-0 items-center gap-2 border-b border-border bg-muted/20 px-3 py-1.5 text-xs">
				<Button
					variant="ghost"
					size="sm"
					className="h-7 w-7 p-0"
					disabled={pageNum <= 1}
					onClick={() => setPageNum((n) => Math.max(1, n - 1))}
				>
					<ChevronLeft className="h-3.5 w-3.5" />
				</Button>
				<span className="tabular-nums text-muted-foreground">
					{pageNum} / {numPages || '?'}
				</span>
				<Button
					variant="ghost"
					size="sm"
					className="h-7 w-7 p-0"
					disabled={numPages > 0 && pageNum >= numPages}
					onClick={() => setPageNum((n) => Math.min(numPages || n, n + 1))}
				>
					<ChevronRight className="h-3.5 w-3.5" />
				</Button>
				<div className="mx-2 h-4 w-px bg-border" />
				<Button
					variant="ghost"
					size="sm"
					className="h-7 w-7 p-0"
					onClick={() => setScale((s) => Math.max(0.25, s - 0.25))}
				>
					<ZoomOut className="h-3.5 w-3.5" />
				</Button>
				<span className="tabular-nums text-muted-foreground">{Math.round(scale * 100)}%</span>
				<Button
					variant="ghost"
					size="sm"
					className="h-7 w-7 p-0"
					onClick={() => setScale((s) => Math.min(3, s + 0.25))}
				>
					<ZoomIn className="h-3.5 w-3.5" />
				</Button>
				<div className="mx-2 h-4 w-px bg-border" />
				<Search className="h-3.5 w-3.5 text-muted-foreground" />
				<input
					value={search}
					onChange={(e) => setSearch(e.target.value)}
					placeholder="Find in page…"
					className="h-7 w-40 rounded border border-border bg-background px-2 text-xs outline-none focus:ring-1 focus:ring-primary"
				/>
				<div className="flex-1" />
				<Button
					variant="ghost"
					size="sm"
					className="h-7 gap-1.5 px-2"
					title={isBrowserHost() ? 'Download' : 'Open in default app'}
					onClick={() => {
						if (state.kind !== 'ready') return;
						// Browser: the bytes are already loaded, hand them straight to the
						// browser. Desktop: let the OS open the file.
						if (isBrowserHost()) {
							saveBlobAs(
								new Blob([state.data as BlobPart], { type: 'application/pdf' }),
								basename(path)
							);
							return;
						}
						openLocalPath(path, { kind: 'file' }).catch((e) =>
							toast({
								label: `Could not open ${basename(path)}: ${e instanceof Error ? e.message : String(e)}`,
								variant: 'error',
							})
						);
					}}
				>
					<Download className="h-3.5 w-3.5" />
					{isBrowserHost() ? 'Download' : 'Open'}
				</Button>
			</div>
			<div className="flex-1 overflow-auto bg-muted/40 px-4 py-4">
				<div className="mx-auto flex flex-col items-center gap-4">
					<Document
						file={file}
						onLoadSuccess={({ numPages: n }) => setNumPages(n)}
						loading={
							<div className="text-xs text-muted-foreground">
								<Loader2 className="mr-2 inline h-3 w-3 animate-spin" />
								Parsing…
							</div>
						}
						error={<div className="text-xs text-destructive">Failed to load PDF.</div>}
					>
						<Page
							pageNumber={pageNum}
							scale={scale}
							renderAnnotationLayer
							renderTextLayer
							customTextRenderer={
								search
									? ({ str }: { str: string }) =>
											str.replace(
												new RegExp(escapeRegExp(search), 'gi'),
												(m) => `<mark>${m}</mark>`
											)
									: undefined
							}
						/>
					</Document>
				</div>
			</div>
		</div>
	);
}

function escapeRegExp(s: string): string {
	return s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}
