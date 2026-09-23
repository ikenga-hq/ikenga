// HOSTILE FIXTURE — do not "fix" this file. The WAVES literal is NOT JSON: it
// carries a computed value with a side effect and an IIFE. Under the old
// `new Function(...)` path both would have run. The importer must now refuse
// with a typed `WorkflowImportError` (`code: 'waves-not-json'`) and leave the
// side effects unperformed (WP-31 review fix, Round 29).

export const meta = {
  name: 'hostile-computed-plan',
  description: 'WAVES built by evaluation, not by JSON.stringify',
}

const WAVES = [
  ((globalThis.__IMPORTER_PWNED_COMPUTED__ = true), { title: 'Wave 1', gate: null, wps: [] }),
  (function () {
    globalThis.__IMPORTER_PWNED_IIFE__ = true
    return { title: 'Wave 2', gate: null, wps: [] }
  })(),
]
