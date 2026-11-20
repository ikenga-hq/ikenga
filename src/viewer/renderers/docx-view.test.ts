// @vitest-environment jsdom
import { describe, expect, it } from 'vitest';
import { sanitizeDocxHtml } from './docx-view';

describe('sanitizeDocxHtml', () => {
	it('keeps document structure', () => {
		const out = sanitizeDocxHtml('<h1>Title</h1><p>Hi <strong>there</strong></p><ul><li>a</li></ul>');
		expect(out).toContain('<h1>Title</h1>');
		expect(out).toContain('<strong>there</strong>');
		expect(out).toContain('<li>a</li>');
	});
	it('strips scripts and event handlers', () => {
		const out = sanitizeDocxHtml('<p onclick="x()">a</p><script>alert(1)</script><img src="data:image/png;base64,AAAA" onerror="x()">');
		expect(out).not.toContain('script');
		expect(out).not.toContain('onclick');
		expect(out).not.toContain('onerror');
	});
	it('keeps data: images and drops remote ones', () => {
		const out = sanitizeDocxHtml('<img src="data:image/png;base64,AAAA"><img src="https://evil.test/x.png">');
		expect(out).toContain('data:image/png');
		expect(out).not.toContain('evil.test');
	});
	it('drops javascript: links', () => {
		const out = sanitizeDocxHtml('<a href="javascript:alert(1)">x</a>');
		expect(out).not.toContain('javascript:');
	});
});
