// PinIcon name resolution (WP-03): seeded package pins carry the manifest's
// `ui.views[0].icon`, spelled either way; unknown names fall back.

import { render } from '@testing-library/react';
import { Pin } from 'lucide-react';
import { describe, expect, it } from 'vitest';
import { normalizeLucideName, PinIcon } from './pin-icon';

describe('normalizeLucideName', () => {
	it('accepts kebab-case and converts Pascal / camel / snake case', () => {
		expect(normalizeLucideName('layout-dashboard')).toBe('layout-dashboard');
		expect(normalizeLucideName('LayoutDashboard')).toBe('layout-dashboard');
		expect(normalizeLucideName('layoutDashboard')).toBe('layout-dashboard');
		expect(normalizeLucideName('git_branch')).toBe('git-branch');
		expect(normalizeLucideName(' Box ')).toBe('box');
	});

	it('returns null for unknown, empty or missing names', () => {
		expect(normalizeLucideName('not-an-icon-name')).toBeNull();
		expect(normalizeLucideName('')).toBeNull();
		expect(normalizeLucideName(null)).toBeNull();
		expect(normalizeLucideName(undefined)).toBeNull();
	});
});

describe('PinIcon', () => {
	it('renders the fallback glyph for an unknown lucide name instead of nothing', () => {
		const { container } = render(
			<PinIcon iconLucide="NoSuchIcon" iconEmoji={null} Fallback={Pin} />
		);
		expect(container.querySelector('svg')).not.toBeNull();
	});

	it('prefers the emoji when the lucide name is unknown', () => {
		const { container } = render(<PinIcon iconLucide="NoSuchIcon" iconEmoji="🎵" Fallback={Pin} />);
		expect(container.textContent).toBe('🎵');
	});
});
