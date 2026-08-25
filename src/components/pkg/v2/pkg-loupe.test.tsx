// Render tests for <PkgLoupe />. Three things worth pinning:
//   1. The head shows name + version + id when a row is supplied.
//   2. Switching tabs swaps the body content.
//   3. The footer action set varies by state (registry → Install,
//      outdated → Update, trust-pending → Approve, default → Disable/
//      Uninstall).
//
// The Loupe is rendered inside a shadcn Sheet (Radix Dialog), so we
// don't pass it `open={true}` straight — Radix portals through the DOM.
// React-testing-library queries can still find the rendered content via
// the global document body.

import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import type { ReactNode } from 'react';

import { PkgLoupe } from './pkg-loupe';
import type { PkgRowV2 } from '@/lib/pkgs/use-derived';

vi.mock('@/lib/tauri-cmd', async (orig) => {
	const actual = await (orig() as Promise<Record<string, unknown>>);
	return {
		...actual,
		pkgKernelStatus: vi.fn(async () => ({
			installed: [],
			registries: {},
			api_version: 1,
		})),
	};
});

afterEach(cleanup);

function withQuery(ui: ReactNode) {
	const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
	return <QueryClientProvider client={client}>{ui}</QueryClientProvider>;
}

function makeRow(overrides: Partial<PkgRowV2> = {}): PkgRowV2 {
	return {
		id: 'com.test.x',
		name: 'Test Pkg',
		version: '0.1.0',
		origin: 'user',
		kind: 'ui',
		state: 'idle',
		enabled: true,
		desc: 'A test pkg with some routes.',
		installPath: '~/.config/ikenga/pkgs/com.test.x',
		installedAt: 1_700_000_000,
		latest: null,
		scopes: ['fs:read:workspace', 'shell.execute:bun'],
		routes: ['x/main'],
		sidecars: [],
		trust: null,
		violations: [],
		screenshots: [],
		installed: null,
		manifest: null,
		registryEntry: null,
		...overrides,
	};
}

describe('PkgLoupe — head', () => {
	it('renders name + version + id when open', () => {
		render(withQuery(<PkgLoupe row={makeRow()} open onOpenChange={() => {}} />));
		expect(screen.getByText('Test Pkg')).toBeTruthy();
		expect(screen.getByText('v0.1.0')).toBeTruthy();
		expect(screen.getByText('com.test.x')).toBeTruthy();
	});

	it('renders nothing in the sheet when row is null', () => {
		render(withQuery(<PkgLoupe row={null} open onOpenChange={() => {}} />));
		// No tab buttons rendered.
		expect(screen.queryByRole('button', { name: /Overview/i })).toBeNull();
	});

	it('shows the Permissions tab count', () => {
		render(
			withQuery(
				<PkgLoupe row={makeRow({ scopes: ['a', 'b', 'c'] })} open onOpenChange={() => {}} />
			)
		);
		// "Permissions" label + count "3" appear together
		const permsTab = screen.getByRole('tab', { name: /Permissions/i });
		expect(within(permsTab).getByText('3')).toBeTruthy();
	});

	it('flags Trust tab as "pending" when state needs approval', () => {
		render(
			withQuery(
				<PkgLoupe
					row={makeRow({
						trust: {
							pkg_id: 'com.test.x',
							version: '0.1.0',
							state: 'needs_approval',
							perms: { shell_execute: [], fs_write_outside_sandbox: [], net: [], vault_keys: [] },
							last_granted_at_ms: null,
							change_reason: null,
							auto_trusted: false,
						},
					})}
					open
					onOpenChange={() => {}}
				/>
			)
		);
		const trustTab = screen.getByRole('tab', { name: /Trust/i });
		expect(within(trustTab).getByText(/pending/i)).toBeTruthy();
	});
});

describe('PkgLoupe — tab switching', () => {
	it('starts on Overview by default', () => {
		render(withQuery(<PkgLoupe row={makeRow()} open onOpenChange={() => {}} />));
		// Overview body shows the "about" section label.
		expect(screen.getByText(/^about$/i)).toBeTruthy();
	});

	it('respects an initial tab prop', () => {
		render(withQuery(<PkgLoupe row={makeRow()} tab="manifest" open onOpenChange={() => {}} />));
		// Manifest tab renders a manifest.json header. Use a regex with `i`
		// flag because the label includes the pkg id which is lowercase.
		expect(screen.getByText(/manifest\.json/i)).toBeTruthy();
	});

	it('clicking a tab swaps the body', async () => {
		const user = userEvent.setup();
		render(withQuery(<PkgLoupe row={makeRow()} open onOpenChange={() => {}} />));
		// Start: Overview is showing.
		expect(screen.getByText(/^about$/i)).toBeTruthy();
		// Click Manifest tab.
		await user.click(screen.getByRole('tab', { name: /Manifest/i }));
		// Manifest body now visible; about-section is gone.
		expect(screen.getByText(/manifest\.json/i)).toBeTruthy();
		expect(screen.queryByText(/^about$/i)).toBeNull();
	});

	it('Permissions tab renders the scope risk classification', async () => {
		const user = userEvent.setup();
		render(
			withQuery(
				<PkgLoupe
					row={makeRow({ scopes: ['fs:write:.company/content', 'fs:read:workspace'] })}
					open
					onOpenChange={() => {}}
				/>
			)
		);
		await user.click(screen.getByRole('tab', { name: /Permissions/i }));
		// The high-risk scope renders the "risk: high" pill from the
		// classifier; low-risk gets "risk: low".
		expect(screen.getByText('fs:write:.company/content')).toBeTruthy();
		expect(screen.getByText('fs:read:workspace')).toBeTruthy();
		expect(screen.getByText(/risk: high/i)).toBeTruthy();
		expect(screen.getByText(/risk: low/i)).toBeTruthy();
	});
});

describe('PkgLoupe — footer actions', () => {
	it('default state shows Disable (no Uninstall when no callback)', () => {
		render(withQuery(<PkgLoupe row={makeRow()} open onOpenChange={() => {}} />));
		expect(screen.getByRole('button', { name: /Disable/i })).toBeTruthy();
		expect(screen.queryByRole('button', { name: /Uninstall/i })).toBeNull();
	});

	it('default state with onUninstall shows both Uninstall + Disable', () => {
		render(
			withQuery(<PkgLoupe row={makeRow()} open onOpenChange={() => {}} onUninstall={() => {}} />)
		);
		expect(screen.getByRole('button', { name: /Uninstall/i })).toBeTruthy();
		expect(screen.getByRole('button', { name: /Disable/i })).toBeTruthy();
	});

	it('builtin pkg suppresses Uninstall even when callback is set', () => {
		render(
			withQuery(
				<PkgLoupe
					row={makeRow({ origin: 'builtin' })}
					open
					onOpenChange={() => {}}
					onUninstall={() => {}}
				/>
			)
		);
		expect(screen.queryByRole('button', { name: /Uninstall/i })).toBeNull();
	});

	it('registry state shows Install action', () => {
		const onInstall = vi.fn();
		render(
			withQuery(
				<PkgLoupe
					row={makeRow({ origin: 'registry', state: 'not-installed' })}
					open
					onOpenChange={() => {}}
					onInstall={onInstall}
				/>
			)
		);
		expect(screen.getByRole('button', { name: /Install/i })).toBeTruthy();
	});

	it('outdated state shows Update action', () => {
		render(
			withQuery(
				<PkgLoupe
					row={makeRow({ version: '0.1.0', latest: '0.2.0' })}
					open
					onOpenChange={() => {}}
					onUpdate={() => {}}
				/>
			)
		);
		expect(screen.getByRole('button', { name: /Update to v0\.2\.0/i })).toBeTruthy();
	});

	it('trust-pending state shows Approve action', () => {
		render(
			withQuery(
				<PkgLoupe
					row={makeRow({
						trust: {
							pkg_id: 'com.test.x',
							version: '0.1.0',
							state: 'needs_approval',
							perms: { shell_execute: [], fs_write_outside_sandbox: [], net: [], vault_keys: [] },
							last_granted_at_ms: null,
							change_reason: null,
							auto_trusted: false,
						},
					})}
					open
					onOpenChange={() => {}}
				/>
			)
		);
		expect(screen.getByRole('button', { name: /Approve v0\.1\.0/i })).toBeTruthy();
	});
});

describe('PkgLoupe — engine installs (Track E)', () => {
	it('renders nothing when no engine has touched the pkg', () => {
		render(withQuery(<PkgLoupe row={makeRow()} open onOpenChange={() => {}} />));
		// Empty registries snapshot → section is hidden entirely.
		expect(screen.queryByText(/engine installs/i)).toBeNull();
	});

	it('surfaces engine MCP fan-out + asset entries from the kernel snapshot', async () => {
		const { pkgKernelStatus } = await import('@/lib/tauri-cmd');
		(pkgKernelStatus as unknown as ReturnType<typeof vi.fn>).mockResolvedValueOnce({
			installed: [],
			api_version: 1,
			registries: {
				mcp: {
					count: 1,
					entries: [],
					config_path: '~/.claude.json',
					adapter_reports: {
						'com.test.x': {
							'claude-code': {
								wrote: ['~/.claude.json#mcpServers.pkg-test-x-foo'],
								skipped: [],
								warnings: ['secret-bearing env var refused: FOO_API_KEY'],
							},
						},
					},
				},
				engine_assets: {
					count: 1,
					entries: [
						{
							pkg_id: 'com.test.x',
							engine_id: 'claude-code',
							kind: 'skills',
							source: '/path/to/skills',
							target: '/home/u/.claude/skills/test-x',
						},
					],
				},
			},
		});

		render(withQuery(<PkgLoupe row={makeRow()} open onOpenChange={() => {}} />));

		// Section header.
		await waitFor(() => {
			expect(screen.getByText(/engine installs/i)).toBeTruthy();
		});
		// Engine display name + raw id chip.
		expect(screen.getByText('Claude Code')).toBeTruthy();
		expect(screen.getByText('claude-code')).toBeTruthy();
		// Summary phrases — 1 wrote + 1 asset link.
		expect(screen.getByText(/Wrote 1 MCP server/i)).toBeTruthy();
		expect(screen.getByText(/1 asset link/i)).toBeTruthy();
		// Warning badge surfaces.
		expect(screen.getByText(/1 warning/i)).toBeTruthy();
	});

	it('expands to show wrote / asset paths + warnings', async () => {
		const user = userEvent.setup();
		const { pkgKernelStatus } = await import('@/lib/tauri-cmd');
		(pkgKernelStatus as unknown as ReturnType<typeof vi.fn>).mockResolvedValueOnce({
			installed: [],
			api_version: 1,
			registries: {
				mcp: {
					count: 0,
					entries: [],
					config_path: '~/.claude.json',
					adapter_reports: {
						'com.test.x': {
							'claude-code': {
								wrote: ['~/.claude.json#mcpServers.pkg-test-x-foo'],
								skipped: [],
								warnings: ['warning-line-A'],
							},
						},
					},
				},
				engine_assets: {
					count: 1,
					entries: [
						{
							pkg_id: 'com.test.x',
							engine_id: 'claude-code',
							kind: 'skills',
							source: '/src/skills',
							target: '/home/u/.claude/skills/test-x',
						},
					],
				},
			},
		});

		render(withQuery(<PkgLoupe row={makeRow()} open onOpenChange={() => {}} />));

		await waitFor(() => {
			expect(screen.getByText('Claude Code')).toBeTruthy();
		});

		// Engine row trigger acts as a button. Click it to expand.
		await user.click(screen.getByText('Claude Code'));

		expect(screen.getByText('~/.claude.json#mcpServers.pkg-test-x-foo')).toBeTruthy();
		expect(screen.getByText('/home/u/.claude/skills/test-x')).toBeTruthy();
		expect(screen.getByText('warning-line-A')).toBeTruthy();
	});

	it('surfaces asset adapter_reports (Track P) warnings + wrote phrase', async () => {
		// Track P: engine_assets now carries its own adapter_reports bucket
		// from each adapter's install_skills/commands/agents call. Asset
		// warnings should bubble up via the engine card's badge + summary,
		// and the "Wrote N asset link(s)" phrase should be sourced from the
		// asset adapter report (separate from the MCP "Wrote" phrase).
		const user = userEvent.setup();
		const { pkgKernelStatus } = await import('@/lib/tauri-cmd');
		(pkgKernelStatus as unknown as ReturnType<typeof vi.fn>).mockResolvedValueOnce({
			installed: [],
			api_version: 1,
			registries: {
				mcp: {
					count: 0,
					entries: [],
					config_path: '~/.claude.json',
					adapter_reports: {},
				},
				engine_assets: {
					count: 1,
					entries: [
						{
							pkg_id: 'com.test.x',
							engine_id: 'claude-code',
							kind: 'skills',
							source: '/src/skills',
							target: '/home/u/.claude/skills/test-x',
						},
					],
					adapter_reports: {
						'com.test.x': {
							'claude-code': {
								wrote: ['/home/u/.claude/skills/test-x'],
								skipped: [],
								warnings: ['replaced stale symlink at /home/u/.claude/skills/test-x'],
							},
						},
					},
				},
			},
		});

		render(withQuery(<PkgLoupe row={makeRow()} open onOpenChange={() => {}} />));

		await waitFor(() => {
			expect(screen.getByText('Claude Code')).toBeTruthy();
		});
		// Asset-sourced summary phrase uses "Wrote N asset link(s)".
		expect(screen.getByText(/Wrote 1 asset link/i)).toBeTruthy();
		// Warning bubbled up from the asset adapter report.
		expect(screen.getByText(/1 warning/i)).toBeTruthy();

		await user.click(screen.getByText('Claude Code'));
		expect(
			screen.getByText('replaced stale symlink at /home/u/.claude/skills/test-x')
		).toBeTruthy();
	});
});
