// .docx preview (WP-07). mammoth converts the document to semantic HTML, which
// is then run through DOMPurify before it touches the DOM: no scripts, no event
// handlers, images only as data: URLs (mammoth inlines them as such). Loaded
// lazily from auto-router so mammoth stays out of the main chunk.

import { useEffect, useState } from 'react';
import { Loader2 } from 'lucide-react';
import DOMPurify from 'dompurify';
import mammoth from 'mammoth';
import { fsRead } from '@/lib/tauri-cmd';
import { CannotPreviewPlate } from '../chrome/cannot-preview-plate';
import { isPreviewTooLarge } from '../lib/preview-cap';

interface DocxViewProps {
	path: string;
}

const DATA_IMAGE = /^data:image\/(png|jpe?g|gif|webp|bmp|svg\+xml);/i;

let hooked = false;
function ensureHook() {
	if (hooked) return;
	hooked = true;
	DOMPurify.addHook('afterSanitizeAttributes', (node) => {
		if (node.tagName === 'IMG') {
			const src = node.getAttribute('src') ?? '';
			if (!DATA_IMAGE.test(src)) node.remove();
		}
		if (node.tagName === 'A') {
			node.setAttribute('rel', 'noopener noreferrer');
			node.setAttribute('target', '_blank');
		}
	});
}

/** Sanitise converter output. Exported for the test. */
export function sanitizeDocxHtml(html: string): string {
	ensureHook();
	return DOMPurify.sanitize(html, {
		USE_PROFILES: { html: true },
		FORBID_TAGS: ['style', 'script', 'iframe', 'object', 'embed', 'form', 'link', 'meta', 'base', 'svg', 'math'],
		FORBID_ATTR: ['style'],
		ALLOWED_URI_REGEXP: /^(?:https?:|mailto:|data:image\/|#)/i,
		ADD_DATA_URI_TAGS: ['img'],
	});
}

/** Bytes → sanitised HTML. Exported for the test. */
export async function docxToHtml(bytes: Uint8Array): Promise<string> {
	const buf = bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength) as ArrayBuffer;
	const res = await mammoth.convertToHtml({ arrayBuffer: buf });
	return sanitizeDocxHtml(res.value);
}

type State =
	| { kind: 'loading' }
	| { kind: 'ready'; html: string }
	| { kind: 'error'; message: string };

export function DocxView({ path }: DocxViewProps) {
	const [state, setState] = useState<State>({ kind: 'loading' });

	useEffect(() => {
		let cancelled = false;
		setState({ kind: 'loading' });
		fsRead(path)
			.then((res) => docxToHtml(new Uint8Array(res.bytes)))
			.then((html) => {
				if (!cancelled) setState({ kind: 'ready', html });
			})
			.catch((err) => {
				if (cancelled) return;
				setState({ kind: 'error', message: err instanceof Error ? err.message : String(err) });
			});
		return () => {
			cancelled = true;
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
			<CannotPreviewPlate
				path={path}
				reason="This document couldn't be converted for preview. Download it to open it in Word or another app."
			/>
		);
	}
	return (
		<div className="h-full overflow-auto" data-state="docx-ready">
			<div
				className="mx-auto w-full max-w-[72ch] px-8 py-8 text-sm leading-relaxed text-foreground [&_a]:text-primary [&_a]:underline [&_h1]:mb-3 [&_h1]:mt-6 [&_h1]:text-2xl [&_h1]:font-semibold [&_h2]:mb-2 [&_h2]:mt-5 [&_h2]:text-xl [&_h2]:font-semibold [&_h3]:mb-2 [&_h3]:mt-4 [&_h3]:text-lg [&_h3]:font-semibold [&_img]:my-3 [&_img]:max-w-full [&_ol]:my-3 [&_ol]:list-decimal [&_ol]:pl-6 [&_p]:my-3 [&_table]:my-4 [&_table]:border-collapse [&_td]:border [&_td]:border-border [&_td]:px-2 [&_td]:py-1 [&_th]:border [&_th]:border-border [&_th]:px-2 [&_th]:py-1 [&_ul]:my-3 [&_ul]:list-disc [&_ul]:pl-6"
				// biome-ignore lint/security/noDangerouslySetInnerHtml: sanitised by DOMPurify above
				dangerouslySetInnerHTML={{ __html: state.html }}
			/>
		</div>
	);
}
