#!/usr/bin/env bun
//
// pa-db — direct SQLite reader for the PA desktop app's database.
//
// Reads the same SQLite file the Tauri app writes to, *without* requiring
// the app to be running. Useful for agents and scripts that want
// canonical project state (storyboards, render jobs, compositions) but
// don't need the UI.
//
// This is the canonical path for "what is in the database":
//   - iyke = live UI/runtime state (focus, cursor, console, network)
//   - pa-db = persisted data (storyboards, renders, etc.)
//
// Usage:
//   bun run scripts/pa-db.ts storyboards list
//   bun run scripts/pa-db.ts storyboards get <slug>
//   bun run scripts/pa-db.ts storyboards beats <slug>
//   bun run scripts/pa-db.ts renders list [--status=running] [--limit=20]
//   bun run scripts/pa-db.ts renders get <id>
//   bun run scripts/pa-db.ts compositions list
//
// All commands accept --json for machine-readable output.

import { Database } from 'bun:sqlite';
import { homedir } from 'node:os';
import { join } from 'node:path';
import { existsSync, readFileSync, readdirSync } from 'node:fs';

// ── DB path resolution ────────────────────────────────────────────────────

function resolveDbPath(): string {
	const override = process.env.PA_DB_PATH;
	if (override && existsSync(override)) return override;
	const platform = process.platform;
	let base: string;
	if (platform === 'darwin') {
		base = join(homedir(), 'Library', 'Application Support', 'app.ikenga');
	} else if (platform === 'win32') {
		base = join(process.env.LOCALAPPDATA ?? join(homedir(), 'AppData', 'Local'), 'app.ikenga');
	} else {
		base = join(homedir(), '.local', 'share', 'app.ikenga');
	}
	// `ikenga.db` is the live store (renamed from the legacy `pa.db` on first
	// launch — src-tauri/src/lib.rs). Fall back to `pa.db` only when the
	// rename never ran (pre-rename install that hasn't booted since).
	const current = join(base, 'ikenga.db');
	if (existsSync(current)) return current;
	const legacy = join(base, 'pa.db');
	if (existsSync(legacy)) return legacy;
	return current;
}

let cachedDb: Database | null = null;
function db(): Database {
	if (cachedDb) return cachedDb;
	const path = resolveDbPath();
	if (!existsSync(path)) {
		throw new Error(
			`ikenga.db not found at ${path}. Has the desktop app ever started? Set PA_DB_PATH to override.`
		);
	}
	// readonly to avoid contention with the running app.
	cachedDb = new Database(path, { readonly: true });
	return cachedDb;
}

// ── Query API (also importable as a module) ──────────────────────────────

export interface StoryboardRow {
	id: string;
	title: string;
	blog_post_id: string | null;
	source_kind: string | null;
	current_rung: number;
	composition_id: string | null;
	exported_at: number | null;
	created_at: number;
	updated_at: number;
}

export interface StoryboardBeatRow {
	id: string;
	storyboard_id: string;
	index_in_board: number;
	label: string;
	time_start: number;
	time_end: number;
	frame_start: number;
	frame_end: number;
	narration_excerpt: string | null;
	intent: string | null;
	// Rung statuses are flattened columns; we expose them as an object.
	rungs: Record<string, unknown>;
}

export interface RenderJobRow {
	id: string;
	composition_id: string;
	props: unknown;
	output_path: string;
	status: string;
	progress: number;
	started_at: number | null;
	completed_at: number | null;
	error: string | null;
	created_at: number;
}

export function listStoryboards(): StoryboardRow[] {
	return db()
		.query(
			`SELECT id, title, blog_post_id, source_kind, current_rung, composition_id,
              exported_at, created_at, updated_at
         FROM storyboards
         ORDER BY updated_at DESC`
		)
		.all() as StoryboardRow[];
}

export function getStoryboard(slug: string): StoryboardRow | null {
	return (
		(db()
			.query(
				`SELECT id, title, blog_post_id, source_kind, current_rung, composition_id,
                exported_at, created_at, updated_at
           FROM storyboards
           WHERE id = ?`
			)
			.get(slug) as StoryboardRow | null) ?? null
	);
}

export function listStoryboardBeats(slug: string): StoryboardBeatRow[] {
	const cols = db().query(`PRAGMA table_info(storyboard_beats)`).all() as Array<{ name: string }>;
	const rungCols = cols
		.map((c) => c.name)
		.filter((n) => /^[012]_/.test(n) || n.startsWith('rung_'));
	const stmt = db().query(
		`SELECT * FROM storyboard_beats WHERE storyboard_id = ? ORDER BY index_in_board ASC`
	);
	const rows = stmt.all(slug) as Record<string, unknown>[];
	return rows.map((r) => {
		const rungs: Record<string, unknown> = {};
		for (const col of rungCols) rungs[col] = r[col];
		return {
			id: r.id as string,
			storyboard_id: r.storyboard_id as string,
			index_in_board: r.index_in_board as number,
			label: r.label as string,
			time_start: r.time_start as number,
			time_end: r.time_end as number,
			frame_start: r.frame_start as number,
			frame_end: r.frame_end as number,
			narration_excerpt: (r.narration_excerpt as string) ?? null,
			intent: (r.intent as string) ?? null,
			rungs,
		};
	});
}

export function listRenderJobs(opts: { status?: string; limit?: number } = {}): RenderJobRow[] {
	const where = opts.status ? `WHERE status = ?` : '';
	const limit = Math.max(1, Math.min(opts.limit ?? 50, 500));
	const stmt = db().query(
		`SELECT id, composition_id, props, output_path, status, progress, started_at,
            completed_at, error, created_at
       FROM render_jobs ${where}
       ORDER BY created_at DESC
       LIMIT ${limit}`
	);
	const rows = (opts.status ? stmt.all(opts.status) : stmt.all()) as Array<
		Omit<RenderJobRow, 'props'> & { props: string }
	>;
	return rows.map((r) => ({ ...r, props: safeJSON(r.props) }));
}

export function getRenderJob(id: string): RenderJobRow | null {
	const r = db()
		.query(
			`SELECT id, composition_id, props, output_path, status, progress, started_at,
              completed_at, error, created_at
         FROM render_jobs WHERE id = ?`
		)
		.get(id) as (Omit<RenderJobRow, 'props'> & { props: string }) | null;
	if (!r) return null;
	return { ...r, props: safeJSON(r.props) };
}

function safeJSON(s: string): unknown {
	if (!s) return null;
	try {
		return JSON.parse(s);
	} catch {
		return s;
	}
}

// ── Compositions (read from the engine's TSX source) ─────────────────────
//
// Compositions self-register via defineComposition() in
// src/compositions/*.tsx. Walk the tree to enumerate them — fast and
// avoids a build step.

export interface CompositionRow {
	slug: string;
	file: string;
	width?: number;
	height?: number;
	fps?: number;
	durationInFrames?: number;
}

/**
 * Scan a directory of Remotion `.tsx` compositions.
 *
 * The default used to be royalti-video-engine/src/compositions, which was
 * retired on 2026-09-08 (WP-16) and deleted. The `existsSync` guard below
 * meant this did not throw -- it just returned [] forever, so the CLI reported
 * "no compositions" on a machine that had them and nothing indicated why.
 *
 * There is no honest replacement default: compositions are no longer a global
 * directory. Under the Studio Remotion lane a composition is a CELL, living at
 * `<project>/cells/<rung>/<uid>/*.tsx`, so the root is per-project and the
 * caller has to say which. `PA_COMPOSITIONS_DIR` covers scripted use; anything
 * else must pass `rootOverride`.
 */
export function listCompositions(rootOverride?: string): CompositionRow[] {
	const engineRoot = rootOverride ?? process.env.PA_COMPOSITIONS_DIR;
	if (!engineRoot) {
		console.error(
			'listCompositions: no root given. Compositions moved to Studio project cells ' +
				'(<project>/cells/<rung>/<uid>/*.tsx) when royalti-video-engine was retired. ' +
				'Pass a root or set PA_COMPOSITIONS_DIR.',
		);
		return [];
	}
	if (!existsSync(engineRoot)) {
		console.error(`listCompositions: ${engineRoot} does not exist`);
		return [];
	}
	const out: CompositionRow[] = [];
	for (const entry of readdirSync(engineRoot, { withFileTypes: true })) {
		if (!entry.isFile() || !entry.name.endsWith('.tsx')) continue;
		const file = join(engineRoot, entry.name);
		const src = readFileSync(file, 'utf8');
		// Parse defineComposition({ id, ... }) — first occurrence per file.
		const idMatch = src.match(/defineComposition\s*\(\s*\{[^}]*?id\s*:\s*["']([^"']+)["']/s);
		if (!idMatch) continue;
		const slug = idMatch[1];
		const width = num(src.match(/width\s*:\s*(\d+)/));
		const height = num(src.match(/height\s*:\s*(\d+)/));
		const fps = num(src.match(/fps\s*:\s*(\d+)/));
		const durationInFrames = num(src.match(/durationInFrames\s*:\s*(\d+)/));
		out.push({ slug, file, width, height, fps, durationInFrames });
	}
	return out.sort((a, b) => a.slug.localeCompare(b.slug));
}

function num(m: RegExpMatchArray | null): number | undefined {
	if (!m) return undefined;
	const n = Number(m[1]);
	return Number.isFinite(n) ? n : undefined;
}

// ── CLI ───────────────────────────────────────────────────────────────────

function isMain(): boolean {
	return import.meta.main;
}

function parseFlags(argv: string[]): { args: string[]; flags: Record<string, string | true> } {
	const args: string[] = [];
	const flags: Record<string, string | true> = {};
	for (const a of argv) {
		if (a.startsWith('--')) {
			const [k, v] = a.slice(2).split('=', 2);
			flags[k] = v ?? true;
		} else {
			args.push(a);
		}
	}
	return { args, flags };
}

function out(value: unknown, asJson: boolean) {
	if (asJson) {
		console.log(JSON.stringify(value, null, 2));
	} else if (Array.isArray(value)) {
		if (value.length === 0) {
			console.log('(empty)');
			return;
		}
		for (const row of value as Record<string, unknown>[]) {
			const cells = Object.entries(row)
				.filter(([, v]) => typeof v !== 'object' || v === null)
				.map(([k, v]) => `${k}=${v}`);
			console.log(cells.join('  '));
		}
	} else if (value === null || value === undefined) {
		console.log('(null)');
	} else {
		console.log(JSON.stringify(value, null, 2));
	}
}

function usage(): never {
	const lines = [
		'Usage: bun run scripts/pa-db.ts <area> <verb> [args...] [--json]',
		'',
		'Areas:',
		'  storyboards list',
		'  storyboards get <slug>',
		'  storyboards beats <slug>',
		'  renders list [--status=queued|running|complete|failed|cancelled] [--limit=N]',
		'  renders get <id>',
		'  compositions list',
		'',
		'Env: PA_DB_PATH overrides the default SQLite path.',
	];
	console.error(lines.join('\n'));
	process.exit(2);
}

if (isMain()) {
	const argv = process.argv.slice(2);
	const { args, flags } = parseFlags(argv);
	const asJson = flags.json === true;
	const [area, verb, ...rest] = args;
	if (!area || !verb) usage();
	try {
		switch (`${area}.${verb}`) {
			case 'storyboards.list':
				out(listStoryboards(), asJson);
				break;
			case 'storyboards.get': {
				const slug = rest[0];
				if (!slug) usage();
				out(getStoryboard(slug), asJson);
				break;
			}
			case 'storyboards.beats': {
				const slug = rest[0];
				if (!slug) usage();
				out(listStoryboardBeats(slug), asJson);
				break;
			}
			case 'renders.list':
				out(
					listRenderJobs({
						status: typeof flags.status === 'string' ? flags.status : undefined,
						limit: typeof flags.limit === 'string' ? Number(flags.limit) : undefined,
					}),
					asJson
				);
				break;
			case 'renders.get': {
				const id = rest[0];
				if (!id) usage();
				out(getRenderJob(id), asJson);
				break;
			}
			case 'compositions.list':
				out(listCompositions(), asJson);
				break;
			default:
				usage();
		}
	} catch (err) {
		console.error(`error: ${err instanceof Error ? err.message : String(err)}`);
		process.exit(1);
	}
}
