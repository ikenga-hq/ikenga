import { useEffect, useState } from 'react';
import { File, Loader2 } from 'lucide-react';
import { Button } from '@/components/ui/button';
import { fsStat } from '@/lib/tauri-cmd';
import { basename } from '../lib/path';
import { formatBytes } from '../lib/preview-cap';
import { downloadLabel, useDownloadFile } from '../chrome/download-file';

interface UnknownViewProps {
	path: string;
	mime?: string;
}

export function UnknownView({ path, mime }: UnknownViewProps) {
	const [size, setSize] = useState<number | null>(null);
	const [resolvedMime, setResolvedMime] = useState<string>(mime ?? 'application/octet-stream');
	const download = useDownloadFile(path);

	// A stat, not a read: learning the size must not pull the whole file over
	// the wire (a .docx or .pptx can be hundreds of MB).
	useEffect(() => {
		let cancelled = false;
		fsStat(path)
			.then((res) => {
				if (cancelled) return;
				setSize(res.size);
				if (!mime) setResolvedMime(res.mime);
			})
			.catch(() => {
				// Size and MIME are decoration; the Download button still works.
			});
		return () => {
			cancelled = true;
		};
	}, [path, mime]);

	return (
		<div className="flex h-full flex-col items-center justify-center gap-3 p-6 text-center text-xs text-muted-foreground">
			<File className="h-10 w-10" />
			<div className="font-medium text-foreground">{basename(path)}</div>
			<div className="font-mono">
				{resolvedMime}
				{size !== null && ` · ${formatBytes(size)}`}
			</div>
			<div className="break-all font-mono text-[10px]">{path}</div>
			<Button variant="outline" size="sm" disabled={download.busy} onClick={() => void download.run()}>
				{download.busy && <Loader2 className="mr-1.5 h-3.5 w-3.5 animate-spin" />}
				{downloadLabel()}
			</Button>
		</div>
	);
}
