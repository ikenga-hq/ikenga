import { beforeEach, describe, expect, it, vi } from 'vitest';

const h = vi.hoisted(() => ({
	projectList: vi.fn(),
	viewerServe: vi.fn(),
}));

vi.mock('@/lib/tauri-cmd', () => ({
	projectList: h.projectList,
	viewerServe: h.viewerServe,
}));

import { mountViewerRoot } from './viewer-root';

const PROJ = '/home/u/proj';
const page = `${PROJ}/pages/deck/index.html`;
const widening = '<link href="../../_shared/t.css">';

beforeEach(() => {
	h.projectList.mockReset();
	h.viewerServe.mockReset();
	h.viewerServe.mockResolvedValue({ token: 't', url: '/__viewer/t/' });
});

describe('mountViewerRoot', () => {
	it('mounts the project root when the page reaches shared assets', async () => {
		h.projectList.mockResolvedValue([{ root_path: PROJ }]);
		const r = await mountViewerRoot(page, widening);
		expect(h.viewerServe).toHaveBeenCalledWith(PROJ, page);
		expect(r.file).toBe('pages/deck/index.html');
	});

	it('mounts only the page directory for a page in no project', async () => {
		h.projectList.mockResolvedValue([]);
		const r = await mountViewerRoot(page, '<img src="../../../.ssh/id_rsa">');
		expect(h.viewerServe).toHaveBeenCalledWith(`${PROJ}/pages/deck`, page);
		expect(r.file).toBe('index.html');
	});

	it('fails closed when the project list cannot be read', async () => {
		h.projectList.mockRejectedValue(new Error('nope'));
		await mountViewerRoot(page, widening);
		expect(h.viewerServe).toHaveBeenCalledWith(`${PROJ}/pages/deck`, page);
	});

	it('retries with the page directory when the host refuses the widened root', async () => {
		// A project rooted at the home dir: the host bounds the page by its own directory.
		h.projectList.mockResolvedValue([{ root_path: PROJ }]);
		h.viewerServe.mockRejectedValueOnce(new Error('preview root is above the project root'));
		const r = await mountViewerRoot(page, widening);
		expect(h.viewerServe).toHaveBeenCalledTimes(2);
		expect(h.viewerServe).toHaveBeenLastCalledWith(`${PROJ}/pages/deck`, page);
		expect(r.file).toBe('index.html');
	});

	it('does not retry when the page directory itself was refused', async () => {
		h.projectList.mockResolvedValue([]);
		h.viewerServe.mockRejectedValue(new Error('outside allowlist'));
		await expect(mountViewerRoot(page, '')).rejects.toThrow('outside allowlist');
		expect(h.viewerServe).toHaveBeenCalledTimes(1);
	});
});
