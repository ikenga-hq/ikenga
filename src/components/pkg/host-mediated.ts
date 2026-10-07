// Host-mediated "open link" / "download file" for pkg iframes in a browser
// session.
//
// The pkg iframe is sandboxed `allow-scripts allow-same-origin` and, per the
// MCP Apps model, deliberately NOT `allow-downloads` / `allow-popups`: a pkg
// that wants to leave the page or hand the user a file asks the HOST
// (`ui/open-link`, `ui/download-file`), which is the one place the shell can
// validate the URL and tell the user when it fails. Without a host handler a
// pkg's "open PR" link or Studio export silently does nothing in a browser
// (the sandbox swallows the popup and the download).
//
// Widening the sandbox flags instead would let a pkg open any URL or write any
// download without passing through that checkpoint, and the popup would be
// unsandboxed (`allow-popups-to-escape-sandbox`) — so the flags stay as they
// are and these handlers carry the weight. Registered for browser sessions
// only: desktop behaviour is unchanged.

import { toast } from '@/lib/toast';
import { isExternalUrl, openExternalUrl, saveBlobAs } from '@/lib/transport/shims';

export interface HostMediatedResult {
	isError?: boolean;
	[key: string]: unknown;
}

function failed(label: string): HostMediatedResult {
	toast({ label, variant: 'error' });
	return { isError: true };
}

/** `ui/open-link`: only `http(s)` and `mailto`, and a failure is shown. */
export async function hostOpenLink(url: string): Promise<HostMediatedResult> {
	if (!isExternalUrl(url))
		return failed('A package tried to open a link that is not a web address.');
	try {
		await openExternalUrl(url);
		return {};
	} catch (e) {
		return failed(`Could not open the link: ${e instanceof Error ? e.message : String(e)}`);
	}
}

interface DownloadPart {
	type?: string;
	uri?: string;
	name?: string;
	mimeType?: string;
	resource?: { uri?: string; mimeType?: string; text?: string; blob?: string };
}

function fileNameFor(uri: string | undefined, fallback: string): string {
	const last = (uri ?? '').split(/[?#]/)[0]?.split('/').filter(Boolean).pop();
	return last || fallback;
}

function decodeBase64(b64: string): Uint8Array {
	const bin = atob(b64);
	const out = new Uint8Array(bin.length);
	for (let i = 0; i < bin.length; i += 1) out[i] = bin.charCodeAt(i);
	return out;
}

/** `ui/download-file`: embedded contents are saved; a linked resource is
 *  opened if it is a web address. Anything else is reported, not dropped. */
export async function hostDownloadFile(contents: unknown[]): Promise<HostMediatedResult> {
	let saved = 0;
	for (const raw of contents) {
		const part = (raw ?? {}) as DownloadPart;
		try {
			if (part.type === 'resource' && part.resource) {
				const { resource } = part;
				const type = resource.mimeType || 'application/octet-stream';
				const name = fileNameFor(resource.uri, 'download');
				if (typeof resource.blob === 'string') {
					saveBlobAs(new Blob([decodeBase64(resource.blob) as BlobPart], { type }), name);
					saved += 1;
					continue;
				}
				if (typeof resource.text === 'string') {
					saveBlobAs(new Blob([resource.text], { type }), name);
					saved += 1;
					continue;
				}
			} else if (part.type === 'resource_link' && part.uri && isExternalUrl(part.uri)) {
				await openExternalUrl(part.uri);
				saved += 1;
				continue;
			}
		} catch (e) {
			return failed(`Download failed: ${e instanceof Error ? e.message : String(e)}`);
		}
		return failed('A package tried to download a file the browser cannot save.');
	}
	return saved > 0 ? {} : failed('The package sent nothing to download.');
}
