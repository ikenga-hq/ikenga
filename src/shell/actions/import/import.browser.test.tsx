// Browser sessions: "Choose keybindings.json…" must read the file the user
// picked on THEIR machine (hidden <input type=file>), not open the server's
// file dialog.

import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { ActionsSurfaceProps } from '../types';

const host = vi.hoisted(() => ({
	browser: true,
	openDialog: vi.fn(),
	fsRead: vi.fn(),
	parse: vi.fn((text: string) => [{ raw: text }]),
}));

vi.mock('@/lib/transport', async (importOriginal) => ({
	...(await importOriginal<typeof import('@/lib/transport')>()),
	isBrowserHost: () => host.browser,
}));
vi.mock('@/lib/transport/dialog-shim', () => ({ open: host.openDialog }));
vi.mock('@/lib/tauri-cmd', async (importOriginal) => ({
	...(await importOriginal<typeof import('@/lib/tauri-cmd')>()),
	fsRead: host.fsRead,
}));
vi.mock('@/lib/actions/import/package', () => ({
	buildPackageDiff: () => [],
	applyPackageImport: vi.fn(),
}));
vi.mock('@/lib/actions/import/vscode', () => ({
	buildVSCodeDiff: (rules: unknown[]) => [
		{ key: 'r1', kind: 'add', title: `rule x${rules.length}`, detail: 'd' },
	],
	applyVSCodeImport: vi.fn(),
	parseVSCodeKeybindingsText: host.parse,
}));

import { ImportSurface } from './index';

const props = {
	scope: 'user',
	model: { projectRoot: null },
	onNavigate: () => {},
} as unknown as ActionsSurfaceProps;

beforeEach(() => {
	host.browser = true;
	host.openDialog.mockReset();
	host.fsRead.mockReset();
	host.parse.mockClear();
});
afterEach(cleanup);

/** jsdom's File has no `.text()`; give it the real behaviour. */
function makeFile(content: string, name: string): File {
	const file = new File([content], name, { type: 'application/json' });
	Object.defineProperty(file, 'text', { value: async () => content });
	return file;
}

function openVsCodeSource() {
	fireEvent.click(screen.getByRole('tab', { name: /From VS Code keybindings/ }));
}

describe('VS Code keybindings import — Choose file', () => {
	it('reads the chosen file client-side in a browser session', async () => {
		render(<ImportSurface {...props} />);
		openVsCodeSource();
		const input = screen.getByTestId('vscode-file-input') as HTMLInputElement;
		expect(input.type).toBe('file');
		expect(input.accept).toContain('.json');

		const file = makeFile('[{"key":"ctrl+k"}]', 'keybindings.json');
		fireEvent.change(input, { target: { files: [file] } });

		await waitFor(() => expect(screen.getByText('rule x1')).toBeDefined());
		expect(host.parse).toHaveBeenCalledWith('[{"key":"ctrl+k"}]');
		expect(host.openDialog).not.toHaveBeenCalled();
		expect(host.fsRead).not.toHaveBeenCalled();
	});

	it('opens the hidden input instead of the server dialog when the button is clicked', () => {
		render(<ImportSurface {...props} />);
		openVsCodeSource();
		const input = screen.getByTestId('vscode-file-input') as HTMLInputElement;
		const click = vi.spyOn(input, 'click');
		fireEvent.click(screen.getByRole('button', { name: /Choose keybindings\.json/ }));
		expect(click).toHaveBeenCalled();
		expect(host.openDialog).not.toHaveBeenCalled();
	});

	it('shows a parse error from the chosen file', async () => {
		host.parse.mockImplementationOnce(() => {
			throw new Error('not valid JSON');
		});
		render(<ImportSurface {...props} />);
		openVsCodeSource();
		const input = screen.getByTestId('vscode-file-input') as HTMLInputElement;
		fireEvent.change(input, { target: { files: [makeFile('x', 'k.json')] } });
		await waitFor(() => expect(screen.getByRole('alert').textContent).toContain('not valid JSON'));
	});

	it('keeps the native dialog on desktop and renders no file input', async () => {
		host.browser = false;
		host.openDialog.mockResolvedValue(null);
		render(<ImportSurface {...props} />);
		openVsCodeSource();
		expect(screen.queryByTestId('vscode-file-input')).toBeNull();
		fireEvent.click(screen.getByRole('button', { name: /Choose keybindings\.json/ }));
		await waitFor(() => expect(host.openDialog).toHaveBeenCalled());
	});
});
