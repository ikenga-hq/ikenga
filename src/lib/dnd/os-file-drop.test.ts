import { afterEach, describe, expect, it, vi } from 'vitest';
import { BROWSER_DROP_MESSAGE, BROWSER_DROP_NOTICE_ID, initOsFileDrop } from './os-file-drop';

const session = vi.hoisted(() => ({ browser: false, tauri: false }));
vi.mock('@/lib/transport', async (orig) => ({
	...(await orig<typeof import('@/lib/transport')>()),
	isBrowserSession: () => session.browser,
	isTauri: () => session.tauri,
	getCurrentWebview: () => ({ onDragDropEvent: vi.fn(async () => () => {}) }),
}));

function drag(type: 'dragover' | 'drop', types: string[]) {
	const e = new Event(type, { cancelable: true, bubbles: true }) as DragEvent;
	Object.defineProperty(e, 'dataTransfer', { value: { types, dropEffect: 'copy' } });
	document.body.dispatchEvent(e);
	return e;
}

afterEach(() => {
	session.browser = false;
	session.tauri = false;
	document.getElementById(BROWSER_DROP_NOTICE_ID)?.remove();
});

describe('OS file drop in a browser', () => {
	it('cancels Files dragover and drop so the tab does not navigate to the file', async () => {
		session.browser = true;
		const off = await initOsFileDrop();
		const over = drag('dragover', ['Files']);
		expect(over.defaultPrevented).toBe(true);
		// Regression: dropEffect='none' makes Chromium suppress the real `drop`
		// event, so the inline notice never showed. Must be left untouched.
		expect((over.dataTransfer as DataTransfer).dropEffect).not.toBe('none');
		expect(drag('drop', ['Files']).defaultPrevented).toBe(true);
		off();
	});

	it('explains inline where the file was dropped', async () => {
		session.browser = true;
		const off = await initOsFileDrop();
		drag('drop', ['Files']);
		const notice = document.getElementById(BROWSER_DROP_NOTICE_ID);
		expect(notice?.textContent).toBe(BROWSER_DROP_MESSAGE);
		expect(notice?.getAttribute('role')).toBe('status');
		off();
		expect(document.getElementById(BROWSER_DROP_NOTICE_ID)).toBeNull();
	});

	it('leaves in-app drags (no Files type) alone', async () => {
		session.browser = true;
		const off = await initOsFileDrop();
		expect(drag('dragover', ['application/x-ikenga-file']).defaultPrevented).toBe(false);
		expect(drag('drop', ['text/plain']).defaultPrevented).toBe(false);
		expect(document.getElementById(BROWSER_DROP_NOTICE_ID)).toBeNull();
		off();
	});

	it('stops guarding after teardown', async () => {
		session.browser = true;
		const off = await initOsFileDrop();
		off();
		expect(drag('drop', ['Files']).defaultPrevented).toBe(false);
	});

	it('installs no window guard outside a browser session (harness)', async () => {
		const off = await initOsFileDrop();
		expect(drag('drop', ['Files']).defaultPrevented).toBe(false);
		off();
	});

	it('installs no window guard on the desktop (native handler owns drops)', async () => {
		session.tauri = true;
		const off = await initOsFileDrop();
		expect(drag('drop', ['Files']).defaultPrevented).toBe(false);
		off();
	});
});
