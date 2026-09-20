/// <reference types="vitest" />
import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';
import { TanStackRouterVite } from '@tanstack/router-vite-plugin';
import tailwindcss from '@tailwindcss/vite';
import path from 'node:path';

const host = process.env.TAURI_DEV_HOST;

// Must match `DEFAULT_VIEWER_PORT` in src-tauri/src/viewer_server/mod.rs.
// The Rust side honours `IKENGA_VIEWER_PORT` for overrides; if you set that
// env var, set it here too (or before `bun run tauri dev`).
const VIEWER_PORT = Number(process.env.IKENGA_VIEWER_PORT ?? 47821);

function stripDevRoutesPlugin(): import('vite').Plugin {
	return {
		name: 'strip-dev-routes',
		apply: 'build',
		enforce: 'pre',
		transform(code, id) {
			if (!id.replace(/\\/g, '/').includes('src/routeTree.gen')) return null;
			let transformed = code;
			// 1. Remove dev route imports
			transformed = transformed.replace(/^import\s+.*\s+from\s+['"].*\/routes\/dev\/.*['"];?\r?\n/gm, '');
			// 2. Remove const Dev...Route = ...
			transformed = transformed.replace(/^\s*const\s+Dev\w+Route\s*=\s*Dev\w+RouteImport[\s\S]*?\)\s*as\s+any\s*\)\r?\n/gm, '');
			// 3. Remove from rootRouteChildren
			transformed = transformed.replace(/^\s*Dev\w+Route:\s*Dev\w+Route,?\r?\n/gm, '');
			// 4. Remove union types
			transformed = transformed.replace(/^\s*\|\s*['"]\/dev\/[^'"]+['"]\r?\n/gm, '');
			// 5. Remove interface property mappings
			transformed = transformed.replace(/^\s*['"]\/dev\/[^'"]+['"]:\s*typeof\s+Dev\w+Route\r?\n/gm, '');
			// 6. Remove route definition objects in interfaces
			transformed = transformed.replace(/^\s*['"]\/dev\/[^'"]+['"]:\s*\{\s*id:\s*['"]\/dev\/[^'"]+['"][\s\S]*?\n\s*\}\r?\n/gm, '');
			return {
				code: transformed,
				map: null,
			};
		},
	};
}

// https://vitejs.dev/config/
export default defineConfig({
	plugins: [
		TanStackRouterVite({
			routesDirectory: './src/routes',
			generatedRouteTree: './src/routeTree.gen.ts',
		}),
		stripDevRoutesPlugin(),
		react(),
		tailwindcss(),
	],

	resolve: {
		alias: {
			'@': path.resolve(__dirname, './src'),
			// jsdom + workspace hoisting via pnpm can double-load React because
			// lucide-react / @testing-library end up in the parent monorepo's
			// pnpm store while react itself is in shell/node_modules. Pin every
			// import of react/react-dom to shell's copy so React.useContext
			// returns the same value across all loaded modules.
			react: path.resolve(__dirname, './node_modules/react'),
			'react-dom': path.resolve(__dirname, './node_modules/react-dom'),
			'react/jsx-runtime': path.resolve(__dirname, './node_modules/react/jsx-runtime.js'),
			'react/jsx-dev-runtime': path.resolve(__dirname, './node_modules/react/jsx-dev-runtime.js'),
			// langium (pulled transitively by mermaid → streamdown / @lobehub/ui)
			// imports two vscode-jsonrpc deep subpaths that Node resolves fine but
			// Rollup's resolver can't follow through pnpm's nested layout, breaking
			// the production build. Pin both to the hoisted vscode-jsonrpc@8.2.0.
			'vscode-jsonrpc/lib/common/events.js': path.resolve(
				__dirname,
				'../node_modules/vscode-jsonrpc/lib/common/events.js'
			),
			'vscode-jsonrpc/lib/common/cancellation.js': path.resolve(
				__dirname,
				'../node_modules/vscode-jsonrpc/lib/common/cancellation.js'
			),
		},
		dedupe: ['react', 'react-dom'],
	},

	// Vite options tailored for Tauri development
	clearScreen: false,
	server: {
		port: 1420,
		strictPort: true,
		host: host || false,
		hmr: host
			? {
					protocol: 'ws',
					host,
					port: 1421,
				}
			: undefined,
		// Same-origin viewer: proxy /__viewer/* to the Rust viewer server so
		// artifact iframes resolve to the same origin as the shell (Vite at
		// :1420). Without this, modern-screenshot and DOM walk-ins are blocked
		// by the browser's Same-Origin Policy.
		proxy: {
			'/__viewer': {
				target: `http://127.0.0.1:${VIEWER_PORT}`,
				changeOrigin: false,
				ws: false,
			},
			'/__viewer-health': {
				target: `http://127.0.0.1:${VIEWER_PORT}`,
				changeOrigin: false,
			},
		},
		watch: {
			// Allowlist, not denylist. Vite's HMR is what makes Tauri dev tolerable
			// (edit a .tsx → component swaps in place, panes/terminal/chat survive),
			// so we don't want to disable the watch — but every non-source folder
			// we watch is a chance for a stray .html / .css change to trigger a
			// full-page reload (Vite hard-codes full-reload on .html in
			// vite/src/node/server/hmr.ts) or a wasted HMR pass.
			//
			// Only three roots feed Vite's module graph:
			//   - index.html  (the entry)
			//   - src/        (the React app)
			//   - public/     (Vite-served static assets)
			// Everything else (design/, sidecars/, hyperframes-projects/,
			// src-tauri/, .tanstack/, dist/, …) is built or served by something
			// other than Vite. Returning `true` from the function ignores a path.
			ignored: (file) => {
				if (file === __dirname) return false; // chokidar starts at the root
				const rel = file.slice(__dirname.length + 1);
				const top = rel.split(/[\/\\]/)[0];
				return !(top === 'src' || top === 'public' || top === 'index.html');
			},
		},
	},
	envPrefix: ['VITE_', 'TAURI_ENV_*'],
	build: {
		target: 'esnext',
		minify: !process.env.TAURI_ENV_DEBUG ? 'esbuild' : false,
		sourcemap: !!process.env.TAURI_ENV_DEBUG,
		manifest: true,
	},
	optimizeDeps: {
		exclude: ['@tauri-apps/api', '@ikenga/ui-lib'],
	},
	test: {
		include: ['src/**/*.{test,spec}.{ts,tsx}'],
		exclude: ['node_modules', 'dist', 'src-tauri'],
		// jsdom for component-tree tests (testing-library/react needs a DOM).
		// Pure-logic tests don't care; the env is just there if a test wants it.
		environment: 'jsdom',
	},
});
