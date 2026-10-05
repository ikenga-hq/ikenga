/**
 * The Claude model catalog, from the vendored copy of `@ikenga/contract`'s
 * generated `schemas/models.json` that the Rust side embeds too
 * (`src-tauri/src/server/shared/model_catalog.json`). One file, so the
 * terminal launch, the chat spawn and any picker or cost display agree on
 * ids, role defaults and prices.
 *
 * Vendored rather than imported from `@ikenga/contract/models` so the shell
 * builds against a contract release that predates the catalog; the Rust test
 * `vendored_copy_matches_contract` catches drift.
 */
import catalogJson from '../../src-tauri/src/server/shared/model_catalog.json';

/** Launch roles that pick a default model when none is named. */
export type ModelRole = 'chi' | 'pane' | 'plan';

/** Claude Code `--model` tier aliases (also groundwork's tier names). */
export type ModelTierAlias = 'opus' | 'sonnet' | 'haiku' | 'fable';

export interface ModelPricing {
	inPerMtok: number | null;
	outPerMtok: number | null;
	cacheReadPerMtok: number | null;
	/** 5-minute cache write. */
	cacheWritePerMtok: number | null;
}

export interface ModelEntry {
	id: string;
	alias: ModelTierAlias;
	family: string;
	tier: number;
	contextTokens: number;
	pricing: ModelPricing;
	pricingVerifiedAt: string;
	source: string;
	defaultFor?: ModelRole[];
}

export interface ModelCatalog {
	version: number;
	defaultModel: string;
	roles: Record<ModelRole, string>;
	aliases: Record<ModelTierAlias, string>;
	models: ModelEntry[];
}

export const MODEL_CATALOG = catalogJson as unknown as ModelCatalog;

/** Model id used when neither a model nor a role is given. */
export const DEFAULT_MODEL_ID: string = MODEL_CATALOG.defaultModel;

/** Catalog default model id for a launch role. */
export function defaultModelForRole(role: ModelRole): string {
	return MODEL_CATALOG.roles[role];
}

/** Catalog row by exact id or tier alias. */
export function findModel(idOrAlias: string): ModelEntry | undefined {
	return (
		MODEL_CATALOG.models.find((m) => m.id === idOrAlias) ??
		MODEL_CATALOG.models.find((m) => m.alias === idOrAlias)
	);
}

/**
 * The model to pass as `--model`: an explicit model wins, else the role's
 * default, else `null` (no flag — Claude Code's own default applies).
 */
export function resolveClaudeModel(model?: string | null, role?: ModelRole | null): string | null {
	if (model) return model;
	if (role) return MODEL_CATALOG.roles[role] ?? null;
	return null;
}
