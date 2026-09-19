// Browser-mode frame harness (WP-19, DEC-27).
//
// Serves the React frame with plain Vite — none of `bun run dev` / `build`'s
// pre-steps (bun:fetch, iyke:bundle, pw:sidecar:build, …): those produce Tauri
// bundle resources, not anything the renderer imports. The Tauri host is replaced by
// `e2e/fixtures/tauri-mock.ts`, installed per test before navigation.
//
// Scope: everything that renders. Checks that need the real host (project
// switch propagation, PTY dispatch, chi_run, migrations) live in
// `scripts/frame-live-probe.ts` — see `e2e/README.md`.

import { defineConfig, devices } from '@playwright/test';

// Deliberately not 1420 (the Tauri dev port), so the harness can run while a
// dev shell is up.
const PORT = Number(process.env.IKENGA_E2E_PORT ?? 14_210);
const BASE_URL = `http://127.0.0.1:${PORT}`;
const CI = !!process.env.CI;

// `vite build` + `vite preview`, not `vite dev`. The dev server serves ~2,300
// unbundled modules and re-optimises deps it discovers late (@codemirror/*
// via @ikenga/ui-lib, which vite.config.ts excludes from pre-bundling); on
// first load the stale-hash requests hang and the frame never mounts. A
// production bundle loads in seconds and is closer to what ships.
//
// Uses the repo's own vite.config.ts unchanged; output goes under
// node_modules/ so it is gitignored and never collides with `dist/`.
// IKENGA_E2E_REUSE_BUILD=1 skips the rebuild for a fast local re-run.
const VITE = 'node node_modules/vite/bin/vite.js';
const OUT_DIR = 'node_modules/.cache/e2e-dist';
const BUILD = `${VITE} build --outDir ${OUT_DIR} --emptyOutDir --logLevel error`;
const PREVIEW = `${VITE} preview --outDir ${OUT_DIR} --port ${PORT} --strictPort --host 127.0.0.1`;
const SERVER_COMMAND = process.env.IKENGA_E2E_REUSE_BUILD ? PREVIEW : `${BUILD} && ${PREVIEW}`;

export default defineConfig({
	testDir: './e2e',
	testMatch: '**/*.spec.ts',
	// Output lives under node_modules/ so it is already gitignored.
	outputDir: './node_modules/.cache/e2e-results',
	// First navigation pays Vite's cold dependency pre-bundle.
	timeout: 120_000,
	expect: { timeout: 15_000 },
	fullyParallel: false,
	workers: 1,
	forbidOnly: CI,
	retries: CI ? 1 : 0,
	reporter: CI ? [['list'], ['github']] : 'list',
	use: {
		baseURL: BASE_URL,
		trace: 'retain-on-failure',
		screenshot: 'only-on-failure',
		colorScheme: 'dark',
	},
	projects: [
		{
			name: 'chromium',
			use: { ...devices['Desktop Chrome'], viewport: { width: 1440, height: 900 } },
		},
	],
	webServer: {
		command: SERVER_COMMAND,
		url: BASE_URL,
		reuseExistingServer: !CI,
		// Cold `vite build` is ~2 min on a laptop; leave headroom for CI.
		timeout: 480_000,
		stdout: 'ignore',
		stderr: 'pipe',
	},
});
