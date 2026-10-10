import { describe, expect, it } from 'vitest';
import { pickViewerRoot, projectRootOf } from './relative-root';

const PROJ = '/home/u/proj';
const page = `${PROJ}/pages/deck/index.html`;

describe('pickViewerRoot', () => {
	it('keeps the file directory when nothing ascends', () => {
		expect(pickViewerRoot(page, '<img src="a.png">', PROJ)).toEqual({
			root: `${PROJ}/pages/deck`,
			file: 'index.html',
		});
	});

	it('widens to reach shared assets inside the project', () => {
		expect(pickViewerRoot(page, '<link href="../../_shared/t.css">', PROJ)).toEqual({
			root: PROJ,
			file: 'pages/deck/index.html',
		});
		expect(pickViewerRoot(page, '<link href="../_shared/t.css">', PROJ)).toEqual({
			root: `${PROJ}/pages`,
			file: 'deck/index.html',
		});
	});

	it('never goes above the project root', () => {
		const hostile =
			'<img src="../../../../.ssh/id_rsa"><img src="../../../../../../../etc/passwd">';
		expect(pickViewerRoot(page, hostile, PROJ)).toEqual({
			root: PROJ,
			file: 'pages/deck/index.html',
		});
	});

	it('does not widen at all for a page in no project', () => {
		expect(pickViewerRoot(page, '<img src="../../../.ssh/id_rsa">', null)).toEqual({
			root: `${PROJ}/pages/deck`,
			file: 'index.html',
		});
		expect(pickViewerRoot(page, '<img src="../x.png">')).toEqual({
			root: `${PROJ}/pages/deck`,
			file: 'index.html',
		});
	});

	it('does not widen when the boundary does not contain the page', () => {
		expect(pickViewerRoot(page, '<img src="../x.png">', '/home/u/other')).toEqual({
			root: `${PROJ}/pages/deck`,
			file: 'index.html',
		});
	});

	it('a prefix-sharing sibling is not the boundary', () => {
		expect(pickViewerRoot(page, '<img src="../../../x.png">', '/home/u/pro')).toEqual({
			root: `${PROJ}/pages/deck`,
			file: 'index.html',
		});
	});
});

describe('projectRootOf', () => {
	it('picks the deepest containing project', () => {
		expect(projectRootOf(page, [PROJ, `${PROJ}/pages`, '/home/u/other'])).toBe(`${PROJ}/pages`);
	});

	it('is null for a page in no project, a root-less project or /', () => {
		expect(projectRootOf(page, ['/home/u/other', null, undefined, '', '/'])).toBeNull();
	});

	it('does not match a sibling that shares a name prefix', () => {
		expect(projectRootOf(page, ['/home/u/pro'])).toBeNull();
	});
});
