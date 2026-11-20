// "Cannot preview" plate — the one fallback every renderer shows when it can't
// render a file: too large (WP-06, the default), or any other `reason` such as
// an unsupported format or a failed media load (WP-09). Says why and offers the
// same Download the rest of the app uses (WP-05).

import { File, Loader2 } from 'lucide-react';
import { Button } from '@/components/ui/button';
import { basename } from '../lib/path';
import { FS_PREVIEW_MAX_BYTES, formatBytes, previewTooLargeSize } from '../lib/preview-cap';
import { downloadLabel, useDownloadFile } from './download-file';

interface CannotPreviewPlateProps {
	path: string;
	/** Why the file can't be previewed. Omit for the "too large" message. */
	reason?: string;
	/** The refusal (an Error or its message); read for the file size. */
	message?: unknown;
	/** The size when the caller already knows it (e.g. from a stat). */
	size?: number;
}

export function CannotPreviewPlate({ path, reason, message, size }: CannotPreviewPlateProps) {
	const { busy, run } = useDownloadFile(path);
	const bytes = size ?? previewTooLargeSize(message);
	return (
		<div
			data-state={reason ? 'cannot-preview' : 'preview-too-large'}
			className="flex h-full flex-col items-center justify-center gap-3 p-6 text-center text-xs text-muted-foreground"
		>
			<File className="h-10 w-10" />
			<div className="font-medium text-foreground">{basename(path)}</div>
			{reason ? (
				<div className="max-w-sm">{reason}</div>
			) : (
				<div>
					{bytes !== null ? `${formatBytes(bytes)} is ` : 'This file is '}
					too large to preview (limit {formatBytes(FS_PREVIEW_MAX_BYTES)}).
				</div>
			)}
			<Button variant="outline" size="sm" disabled={busy} onClick={() => void run()}>
				{busy && <Loader2 className="mr-1.5 h-3.5 w-3.5 animate-spin" />}
				{downloadLabel()}
			</Button>
		</div>
	);
}
