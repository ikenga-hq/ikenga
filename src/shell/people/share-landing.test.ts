import { beforeEach, describe, expect, it } from 'vitest';
import { consumeShareLanding, joinProjectPath, LAND_KEY, markShareLanding } from './share-landing';

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

	it('is set for an artifact share and consumed exactly once', () => {
		markShareLanding('o/p', 'artifact');
		expect(consumeShareLanding('o/p')).toBe(true);
		expect(consumeShareLanding('o/p')).toBe(false);
	});

	it('is not set for a project share, and not consumed by another share', () => {
		markShareLanding('o/p', 'project');
		expect(consumeShareLanding('o/p')).toBe(false);
		markShareLanding('o/p', 'artifact');
		expect(consumeShareLanding('o/other')).toBe(false);
		expect(sessionStorage.getItem(LAND_KEY)).toBe('o/p');
	});
});
