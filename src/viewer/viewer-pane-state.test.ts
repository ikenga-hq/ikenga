// WP-44 — unit tests for the per-pane D-08 chrome state store (zoom / device
// / variant). Not run in this session (DEC-50 — see the WP-44 report).

import { beforeEach, describe, expect, it } from 'vitest';
import { useViewerPaneState, ZOOM_MAX, ZOOM_MIN } from './viewer-pane-state';

const PANE = 'pane-1';

beforeEach(() => {
	useViewerPaneState.setState({ panes: {} });
});

describe('forPane', () => {
	it('returns the default state for a pane with no entry yet', () => {
		expect(useViewerPaneState.getState().forPane(PANE)).toEqual({
			zoom: 100,
			device: 'full',
			variant: 'default',
		});
	});
});

describe('zoomBy / resetZoom', () => {
	it('adjusts zoom by the given delta', () => {
		useViewerPaneState.getState().zoomBy(PANE, 10);
		expect(useViewerPaneState.getState().forPane(PANE).zoom).toBe(110);
	});

	it('clamps zoom to ZOOM_MAX', () => {
		useViewerPaneState.getState().zoomBy(PANE, 1000);
		expect(useViewerPaneState.getState().forPane(PANE).zoom).toBe(ZOOM_MAX);
	});

	it('clamps zoom to ZOOM_MIN', () => {
		useViewerPaneState.getState().zoomBy(PANE, -1000);
		expect(useViewerPaneState.getState().forPane(PANE).zoom).toBe(ZOOM_MIN);
	});

	it('resetZoom always returns to 100 regardless of prior zoom', () => {
		useViewerPaneState.getState().zoomBy(PANE, 50);
		useViewerPaneState.getState().resetZoom(PANE);
		expect(useViewerPaneState.getState().forPane(PANE).zoom).toBe(100);
	});
});

describe('setDevice / setVariant', () => {
	it('updates device independently of variant', () => {
		useViewerPaneState.getState().setVariant(PANE, 'source');
		useViewerPaneState.getState().setDevice(PANE, '390');
		const s = useViewerPaneState.getState().forPane(PANE);
		expect(s.device).toBe('390');
		expect(s.variant).toBe('source');
	});
});

describe('reset', () => {
	it('drops a pane back to the default state', () => {
		useViewerPaneState.getState().zoomBy(PANE, 20);
		useViewerPaneState.getState().setDevice(PANE, '768');
		useViewerPaneState.getState().reset(PANE);
		expect(useViewerPaneState.getState().forPane(PANE)).toEqual({
			zoom: 100,
			device: 'full',
			variant: 'default',
		});
	});

	it('does not disturb other panes', () => {
		useViewerPaneState.getState().zoomBy('pane-2', 30);
		useViewerPaneState.getState().zoomBy(PANE, 30);
		useViewerPaneState.getState().reset(PANE);
		expect(useViewerPaneState.getState().forPane('pane-2').zoom).toBe(130);
	});
});
