// G-ACTIONS file shapes (plans/shell-ux-rearchitecture/drafts/actions-schema.md,
// FROZEN Round 39) and the WP-50 command payloads. The Rust validator in
// `src-tauri/src/actions/schema.rs` is authoritative; these types mirror it.

export type ActionsScope = 'personal' | 'project';
export type ActionsFileKind = 'actions' | 'keybindings';

export const ACTIONS_SCHEMA = 'urn:ikenga:actions:v1';
export const KEYBINDINGS_SCHEMA = 'urn:ikenga:keybindings:v1';
export const ACTIONS_CHANGED_EVENT = 'actions://changed';

// --- actions.json (§1.2–§1.4, §8.1) ------------------------------------------

export type ChiTarget = 'active' | 'new' | 'engine';

export type ActionRun =
	| { kind: 'chi'; target: ChiTarget; engineId?: string; prompt: string }
	| { kind: 'shell'; command: string; cwd?: string; confirm?: boolean }
	| { kind: 'iyke'; route: string; method?: 'GET' | 'POST' }
	| { kind: 'skill'; skill: string }
	| { kind: 'workflow'; workflow: string }
	| { kind: 'open'; url: string };

export type ActionRunKind = ActionRun['kind'];

/** DEC-55: project actions of these kinds refuse until trusted. */
export const GATED_RUN_KINDS: readonly ActionRunKind[] = ['shell', 'iyke', 'skill', 'workflow'];

/** §8.2: the six `{{name}}` run variables. */
export const RUN_VARIABLES = [
	'file.path',
	'file.name',
	'selection',
	'project.root',
	'pane.url',
	'branch',
] as const;
export type RunVariable = (typeof RUN_VARIABLES)[number];

/** A full menu id (§1.3): `files`, `section/<sectionId>`, `native/<top>`, … */
export type MenuId = string;

export interface Placement {
	at: MenuId;
	/** DEC-62 `when`, evaluated in the menu context (§1.3). */
	when?: string;
	[unknown: string]: unknown;
}

/** A user action (personal or project file). */
export interface UserAction {
	id: string;
	name: string;
	/** Lucide icon name; default `zap`. */
	icon?: string;
	description?: string;
	run: ActionRun;
	placements?: Placement[];
	scope: ActionsScope;
	[unknown: string]: unknown;
}

/** `"---"` is a separator, never an id. */
export type MenuItem = string;

export interface MenuOverride {
	items?: MenuItem[];
	hidden?: string[];
	[unknown: string]: unknown;
}

export interface ActionsDocument {
	$schema?: typeof ACTIONS_SCHEMA;
	version: 1;
	actions?: UserAction[];
	menus?: Record<MenuId, MenuOverride>;
	/** Unknown top-level keys are preserved on write. */
	[unknown: string]: unknown;
}

// --- keybindings.json (§1.5) --------------------------------------------------

export interface KeybindingRule {
	/** §3.1 grammar: `mod+shift+e`, or a two-stroke chord `mod+k mod+r`. */
	key: string;
	/** An action id; a leading `-` makes a negative rule. */
	command: string;
	when?: string;
	/** Default `app`. `os` only in the personal file (DEC-60). */
	scope?: 'app' | 'os';
	platform?: 'mac' | 'other';
	[unknown: string]: unknown;
}

export interface KeybindingsDocument {
	$schema?: typeof KEYBINDINGS_SCHEMA;
	version: 1;
	bindings?: KeybindingRule[];
	[unknown: string]: unknown;
}

export type ActionsFileDocument = ActionsDocument | KeybindingsDocument;

// --- validation (§1.6) --------------------------------------------------------

export type ValidationErrorCode =
	| 'E_JSON'
	| 'E_VERSION'
	| 'E_FIELD'
	| 'E_ID_GRAMMAR'
	| 'E_ID_BUILTIN'
	| 'E_ID_DUP'
	| 'E_SCOPE_MISMATCH'
	| 'E_RUN_KIND'
	| 'E_VAR_UNKNOWN'
	| 'E_KEY_GRAMMAR'
	| 'E_WHEN_SYNTAX'
	| 'E_LOCKED_HIDDEN'
	| 'E_OS_LAYER'
	| 'E_OS_COMMAND'
	| 'E_OS_WHEN'
	| 'E_OS_CHORD';

export type ValidationWarningCode =
	| 'W_UNKNOWN_FIELD'
	| 'W_UNKNOWN_COMMAND'
	| 'W_UNKNOWN_MENU'
	| 'W_UNKNOWN_CONTEXT_KEY'
	| 'W_FOCUS_IN_PLACEMENT'
	| 'W_UNKNOWN_ICON'
	| 'W_NEGATIVE_NOOP';

export interface ValidationIssue<Code extends string = ValidationErrorCode | ValidationWarningCode> {
	code: Code;
	/** JSON pointer into the document (`''` = the root). */
	path: string;
	message: string;
}

export interface Validation {
	errors: ValidationIssue<ValidationErrorCode>[];
	warnings: ValidationIssue<ValidationWarningCode>[];
}

// --- command results ----------------------------------------------------------

export interface ActionsFileState<D extends ActionsFileDocument = ActionsFileDocument> {
	kind: ActionsFileKind;
	scope: ActionsScope;
	path: string;
	/** The file exists. Absent = empty (§1.1). */
	present: boolean;
	/**
	 * The document in force: the file when valid; else the last valid
	 * document read this session (`stale: true`); else null. A malformed file
	 * is surfaced through `validation` / `error`, never wiped.
	 */
	document: D | null;
	stale: boolean;
	validation: Validation;
	/** A read failure outside validation (linked path, I/O). */
	error: string | null;
}

export interface ActionsScopeFiles {
	scope: ActionsScope;
	actions: ActionsFileState<ActionsDocument>;
	keybindings: ActionsFileState<KeybindingsDocument>;
}

export type TrustState = 'not-gated' | 'absent' | 'untrusted' | 'trusted' | 'changed';

export interface KeybindingsTrust {
	/** SHA-256 of the canonical `bindings` array; null when the file is absent. */
	hash: string | null;
	ruleCount: number;
	/** `untrusted` / `changed` ⇒ every project rule is held (DEC-65). */
	state: TrustState;
}

export interface ActionsFilesResult {
	personal: ActionsScopeFiles;
	/** Null when the project has no filesystem root. */
	project: ActionsScopeFiles | null;
	projectId: string | null;
	projectRoot: string | null;
	projectKeybindingsTrust: KeybindingsTrust | null;
	trustError: string | null;
}

export interface ActionsWriteResult {
	/** False when validation refused the document; the file is untouched. */
	written: boolean;
	kind: ActionsFileKind;
	scope: ActionsScope;
	path: string;
	validation: Validation;
}

export interface ActionTrust {
	id: string;
	name: string | null;
	kind: ActionRunKind;
	/** The exact `run` object, for the trust sheet to show verbatim. */
	run: ActionRun;
	/** SHA-256 of the canonical `run` JSON (B-14). */
	hash: string;
	state: TrustState;
}

export interface ActionsTrustStatus {
	projectId: string;
	projectRoot: string;
	actions: ActionTrust[];
	actionsError: string | null;
	keybindings: KeybindingsTrust;
	keybindingsError: string | null;
}

export interface ActionsTrustGrant {
	/** Each hash is the one the user was shown; a stale one refuses the grant. */
	actions?: { id: string; hash: string }[];
	/** The `bindings` hash the user was shown. */
	keybindings?: string | null;
}

export interface ActionsTrustRevoke {
	actionIds?: string[] | null;
	keybindings?: boolean | null;
}

export interface ActionsChangeEvent {
	path: string;
	file: ActionsFileKind;
	scope: ActionsScope;
}
