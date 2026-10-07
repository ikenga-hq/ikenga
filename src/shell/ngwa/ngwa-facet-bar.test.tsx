// The Trust facet on a host that does not evaluate trust (the headless
// daemon): pkgs whose trust is unknown are counted under their own
// "unavailable" chip instead of vanishing from every trust filter.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render } from '@testing-library/react';
import type { NgwaItem } from '@ikenga/contract';
import { markTrustUnavailable } from '@/lib/ngwa/enrichment';
import { mkItem } from '@/routes/ngwa/-ngwa-test-fixtures';
import { DEFAULT_FACETS, NgwaFacetBar, type NgwaFacetsState } from './ngwa-facet-bar';

const REASON = 'trust evaluation is not available on this server: no trust store';

function items(): NgwaItem[] {
	return [
		mkItem({ id: 'com.x.app', kind: 'app', name: 'com.x.app' }),
		mkItem({ id: 'com.x.tool', kind: 'tool', name: 'com.x.tool' }),
		mkItem({ id: 'skill:personal:tidy', kind: 'skill', name: 'tidy' }),
	];
}

function chip(container: HTMLElement, id: string): HTMLButtonElement | null {
	return container.querySelector<HTMLButtonElement>(`[data-trust-facet="${id}"]`);
}

beforeEach(() => {
	try {
		sessionStorage.setItem('ikenga.ngwa.morefilters', '1');
	} catch {
		// ignore
	}
});

afterEach(() => {
	cleanup();
	try {
		sessionStorage.removeItem('ikenga.ngwa.morefilters');
	} catch {
		// ignore
	}
});

describe('NgwaFacetBar — trust facet', () => {
	it('on the desktop (trust evaluated) there is no "unavailable" chip', () => {
		const { container } = render(
			<NgwaFacetBar items={items()} facets={DEFAULT_FACETS} onChange={() => {}} />
		);
		expect(chip(container, 'unsigned')).not.toBeNull();
		expect(chip(container, 'unavailable')).toBeNull();
	});

	it('when the host does not evaluate trust, pkgs are counted under "unavailable" and filterable', () => {
		const onChange = vi.fn<(next: NgwaFacetsState) => void>();
		const marked = markTrustUnavailable(items(), REASON);
		const { container } = render(
			<NgwaFacetBar items={marked} facets={DEFAULT_FACETS} onChange={onChange} />
		);
		const na = chip(container, 'unavailable') as HTMLButtonElement;
		expect(na).not.toBeNull();
		expect(na.textContent).toContain('not available on this server');
		expect(na.querySelector('.n')?.textContent).toBe('2');
		expect(na.disabled).toBe(false);
		// Only the skill is left as "unsigned" (genuinely not applicable).
		expect(chip(container, 'unsigned')?.querySelector('.n')?.textContent).toBe('1');
		// Every item is accounted for across the trust chips.
		const all = chip(container, '*')?.querySelector('.n')?.textContent;
		expect(all).toBe('3');
		fireEvent.click(na);
		expect(onChange).toHaveBeenCalledWith(expect.objectContaining({ trust: 'unavailable' }));
	});

	it('stays offered while it is the active filter', () => {
		const { container } = render(
			<NgwaFacetBar
				items={items()}
				facets={{ ...DEFAULT_FACETS, trust: 'unavailable' }}
				onChange={() => {}}
			/>
		);
		const na = chip(container, 'unavailable') as HTMLButtonElement;
		expect(na).not.toBeNull();
		expect(na.getAttribute('aria-pressed')).toBe('true');
	});
});
