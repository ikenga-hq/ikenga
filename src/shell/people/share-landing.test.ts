import { beforeEach, describe, expect, it } from 'vitest';
import {
	markLayoutRestored,
	resetLayoutRestoredForTests,
	whenLayoutRestored,
} from '@/lib/panes/layout-restored';
import type { ShareSelection } from '@/lib/transport';
import {
	consumeShareLanding,
	joinProjectPath,
	LAND_KEY,
	markShareLanding,
	reloadShareLandingForTests,
	sharedArtifactEntry,
} from './share-landing';

const share = (over: Partial<ShareSelection>): ShareSelection => ({
	projectKey: 'o/p',
	projectId: 'p',
	projectName: 'P',
	ownerUsername: 'o',
	role: 'guest',
	scope: 'artifact',
	artifactPath: 'notes.md',
	...over,
});

describe('sharedArtifactEntry (the Files row of an artifact share)', () => {
	it('is the shared file under the project root', () => {
		expect(sharedArtifactEntry('/srv/proj', share({}))).toMatchObject({
			path: '/srv/proj/notes.md',
			name: 'notes.md',
			isDir: false,
		});
		expect(
			sharedArtifactEntry('/srv/proj/', share({ artifactPath: './docs/a.html' }))
		).toMatchObject({
			path: '/srv/proj/docs/a.html',
			name: 'docs/a.html',
		});
	});

	it('is nothing outside an artifact share', () => {
		expect(sharedArtifactEntry('/srv/proj', null)).toBeNull();
		expect(
			sharedArtifactEntry('/srv/proj', share({ scope: 'project', artifactPath: undefined }))
		).toBeNull();
		expect(sharedArtifactEntry('', share({}))).toBeNull();
	});
});

describe('whenLayoutRestored', () => {
	beforeEach(() => resetLayoutRestoredForTests());

	it('waits for the mark, and resolves at once after it', async () => {
		let done = false;
		const p = whenLayoutRestored(5_000).then((v) => {
			done = true;
			return v;
		});
		await Promise.resolve();
		expect(done).toBe(false);
		markLayoutRestored();
		await expect(p).resolves.toBe(true);
		await expect(whenLayoutRestored(1)).resolves.toBe(true);
	});

	it('gives up after the bound', async () => {
		await expect(whenLayoutRestored(5)).resolves.toBe(false);
	});
});

describe('joinProjectPath', () => {
	it('joins a root and a project-relative path with one slash', () => {
		expect(joinProjectPath('/srv/proj', 'docs/brief.html')).toBe('/srv/proj/docs/brief.html');
		expect(joinProjectPath('/srv/proj/', './docs/brief.html')).toBe('/srv/proj/docs/brief.html');
	});
});

describe('share landing marker', () => {
	beforeEach(() => {
		const store = new Map<string, string>();
		(globalThis as { sessionStorage?: unknown }).sessionStorage = {
			getItem: (k: string) => store.get(k) ?? null,
			setItem: (k: string, v: string) => void store.set(k, v),
			removeItem: (k: string) => void store.delete(k),
		};
	});

	it('is set for an artifact share and consumed exactly once, on the next load', () => {
		reloadShareLandingForTests();
		markShareLanding('o/p', 'artifact');
		reloadShareLandingForTests();
		expect(consumeShareLanding('o/p')).toBe(true);
		expect(consumeShareLanding('o/p')).toBe(false);
		expect(sessionStorage.getItem(LAND_KEY)).toBeNull();
	});

	// Live defect D3: the invite page enters share mode (the mounted banner
	// hears it at once) and marks the landing, then reloads. Consumed on that
	// page, the open died with its reload and the reloaded app never landed.
	it('is not consumed by the page that set it', () => {
		reloadShareLandingForTests();
		markShareLanding('o/p', 'artifact');
		expect(consumeShareLanding('o/p')).toBe(false);
		expect(sessionStorage.getItem(LAND_KEY)).toBe('o/p');
		reloadShareLandingForTests(); // the reload
		expect(consumeShareLanding('o/p')).toBe(true);
	});

	it('is not set for a project share, and not consumed by another share', () => {
		markShareLanding('o/p', 'project');
		reloadShareLandingForTests();
		expect(consumeShareLanding('o/p')).toBe(false);
		markShareLanding('o/p', 'artifact');
		reloadShareLandingForTests();
		expect(consumeShareLanding('o/other')).toBe(false);
		expect(sessionStorage.getItem(LAND_KEY)).toBe('o/p');
	});
});
