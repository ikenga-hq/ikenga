import { useEffect, useState } from 'react';
import { File, Loader2 } from 'lucide-react';
import { Button } from '@/components/ui/button';
import { fsRead } from '@/lib/tauri-cmd';
import { basename } from '../lib/path';
import { downloadLabel, useDownloadFile } from '../chrome/download-file';

interface UnknownViewProps {
	path: string;
	mime?: string;
}

export function UnknownView({ path, mime }: UnknownViewProps) {
	const [size, setSize] = useState<number | null>(null);
	const [resolvedMime, setResolvedMime] = useState<string>(mime ?? 'application/octet-stream');
	const download = useDownloadFile(path);

	useEffect(() => {
		let cancelled = false;
		fsRead(path)
			.then((res) => {
				if (cancelled) return;
				setSize(res.bytes.length);
				if (!mime) setResolvedMime(res.mime);
			})
			.catch(() => {});
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

function formatBytes(n: number): string {
	if (n < 1024) return `${n} B`;
	if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
	if (n < 1024 * 1024 * 1024) return `${(n / 1024 / 1024).toFixed(1)} MB`;
	return `${(n / 1024 / 1024 / 1024).toFixed(2)} GB`;
}
