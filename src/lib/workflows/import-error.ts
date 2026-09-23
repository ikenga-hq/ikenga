/**
 * Typed import failures for the workflow importers (WP-31 review fix, Round 29).
 *
 * The groundwork importer used to recover from a malformed
 * `orchestrate.workflow.js` by handing the source to `new Function(...)` —
 * i.e. by *executing* an imported file. It no longer executes anything: the
 * `const WAVES = [...]` literal groundwork emits is pure JSON
 * (`JSON.stringify(waves, null, 2)`), so it is extracted by string-aware
 * bracket matching and handed to `JSON.parse`. When that fails we surface one
 * of these instead of reaching for an evaluator.
 */
export type WorkflowImportErrorCode =
  /** `const WAVES = [` … the array literal was not found at all. */
  | 'waves-not-found'
  /** The bracketed slice was found but is not valid JSON. */
  | 'waves-not-json';

export class WorkflowImportError extends Error {
  readonly code: WorkflowImportErrorCode;
  readonly sourcePath: string | null;

  constructor(code: WorkflowImportErrorCode, message: string, sourcePath?: string | null) {
    super(message);
    this.name = 'WorkflowImportError';
    this.code = code;
    this.sourcePath = sourcePath ?? null;
  }
}

export function isWorkflowImportError(e: unknown): e is WorkflowImportError {
  return e instanceof WorkflowImportError;
}
