// plans/file-editing (F3): the artifact pane remounts its renderer on every
// disk change (`useArtifactDiskWatch` → `reloadKey`). While an editor in this
// pane is in Edit that remount must not happen — it threw the user out of Edit
// on their own save and silently dropped unsaved edits on an outside write.

import { act, cleanup, render, renderHook, screen } from '@testing-library/react';
import { useEffect } from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { sessionKey, useEditingStore } from '@/lib/editing/editing-store';

const h = vi.hoisted(() => ({
	disk: { changed: false, reloadKey: 0 },
	mounts: 0,
	stopped: false,
}));

vi.mock('@/viewer/auto-router', () => ({
	ViewerRouter: () => {
		useEffect(() => {
			h.mounts++;
		}, []);
		return <div data-testid="viewer" />;
	},
}));
vi.mock('@/viewer/chrome/artifact-info-strip', () => ({
	ArtifactInfoStrip: ({ kind }: { kind: string }) => <div data-testid={`strip-${kind}`} />,
}));
vi.mock('@/viewer/chrome/artifact-stopped-plate', () => ({
	ArtifactStoppedPlate: () => <div data-testid="stopped-plate" />,
}));
vi.mock('@/viewer/renderers/code-view', () => ({
	CodeView: ({ editable }: { editable?: boolean }) => (
		<div data-testid="source-editor" data-editable={String(editable)} />
	),
}));
vi.mock('@/viewer/chrome/use-artifact-disk-watch', () => ({
	useArtifactDiskWatch: () => ({ ...h.disk, dismiss: () => {} }),
}));
vi.mock('@/viewer/chrome/use-viewer-server-health', () => ({
	useViewerServerHealth: () => ({ stopped: h.stopped, restart: () => {} }),
}));
vi.mock('@/viewer/history/version-history-panel', () => ({ VersionHistoryPanel: () => null }));
vi.mock('@/lib/window/detached-surfaces', () => ({ useIsSurfaceDetached: () => false }));
vi.mock('@/lib/window/window-two', () => ({ popOutSurface: vi.fn() }));
vi.mock('@/shell/companion/seat-notice', () => ({ showSeatNotice: vi.fn() }));

import { useViewerPaneState } from '@/viewer/viewer-pane-state';
import { ArtifactView, useEditAwareReloadKey } from './artifact-view';

function setEditing(editing: boolean) {
	act(() => {
		useEditingStore.getState().upsert(sessionKey('/w/a.ts', 'p1'), {
			path: '/w/a.ts',
			paneId: 'p1',
			mounted: true,
			editing,
			dirty: false,
		});
	});
}

beforeEach(() => {
	h.disk = { changed: false, reloadKey: 0 };
	h.mounts = 0;
	h.stopped = false;
	useEditingStore.setState({ sessions: {} });
});
afterEach(cleanup);

describe('useEditAwareReloadKey', () => {
	it('passes bumps through when not editing, swallows them while editing', () => {
		const { result, rerender } = renderHook(({ k, e }) => useEditAwareReloadKey(k, e), {
			initialProps: { k: 0, e: false },
		});
		rerender({ k: 1, e: false });
		expect(result.current).toBe(1);
		rerender({ k: 2, e: true });
		rerender({ k: 3, e: true });
		expect(result.current).toBe(1);
		// Leaving Edit does not replay the swallowed bumps.
		rerender({ k: 3, e: false });
		expect(result.current).toBe(1);
		rerender({ k: 4, e: false });
		expect(result.current).toBe(2);
	});
});

describe('ArtifactView while editing', () => {
	it('a disk change remounts the renderer when no edit session is open', () => {
		const { rerender } = render(<ArtifactView path="/w/a.ts" paneId="p1" />);
		expect(h.mounts).toBe(1);
		h.disk = { changed: true, reloadKey: 1 };
		rerender(<ArtifactView path="/w/a.ts" paneId="p1" />);
		expect(h.mounts).toBe(2);
		expect(screen.getByTestId('strip-changed')).toBeTruthy();
	});

	it('the key is frozen and the changed strip hidden while this pane is editing', () => {
		const { rerender } = render(<ArtifactView path="/w/a.ts" paneId="p1" />);
		setEditing(true);
		h.disk = { changed: true, reloadKey: 1 };
		rerender(<ArtifactView path="/w/a.ts" paneId="p1" />);
		expect(h.mounts).toBe(1);
		expect(screen.queryByTestId('strip-changed')).toBeNull();
	});

	it('an edit session in another pane does not freeze this one', () => {
		const { rerender } = render(<ArtifactView path="/w/a.ts" paneId="p2" />);
		setEditing(true); // pane p1
		h.disk = { changed: true, reloadKey: 1 };
		rerender(<ArtifactView path="/w/a.ts" paneId="p2" />);
		expect(h.mounts).toBe(2);
	});
});

// Regression (plans/file-editing F1): the daemon does not serve `viewer_port`,
// so in a browser an HTML artifact always reads as "viewer server stopped".
// The stopped plate used to replace the whole pane, Open source included, so
// HTML source editing was unreachable in a browser.
describe('ArtifactView — Open source with the viewer server stopped', () => {
	it('still shows the source editor; only the rendered half shows the plate', async () => {
		h.stopped = true;
		render(<ArtifactView path="/w/page.html" paneId="p1" />);
		expect(screen.getByTestId('stopped-plate')).toBeTruthy();
		act(() => useViewerPaneState.getState().setVariant('p1', 'source'));
		const editor = await screen.findByTestId('source-editor');
		expect(editor.getAttribute('data-editable')).toBe('true');
		expect(screen.getByTestId('stopped-plate')).toBeTruthy();
		expect(screen.queryByTestId('viewer')).toBeNull();
	});
});
