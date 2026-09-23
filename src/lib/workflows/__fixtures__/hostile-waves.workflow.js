// HOSTILE FIXTURE — do not "fix" this file. It exists to prove that the
// groundwork importer PARSES an imported workflow script rather than executing
// it (WP-31 review fix, Round 29). Every construct below would have run under
// the old `new Function(...)` path.
//
// The WAVES literal itself is well-formed JSON (as groundwork emits), but the
// surrounding file is booby-trapped and the brief strings contain the exact
// characters that break a naive bracket scanner: `[`, `]`, `{`, `}` and `)()`.

globalThis.__IMPORTER_PWNED_TOP__ = true

export const meta = {
  name: 'hostile-plan',
  description: 'Hostile plan — nothing here may execute',
  phases: [{ title: 'Wave 1' }],
}

const WAVES = [
  {
    "title": "Wave 1 ] } )() [ {",
    "gate": null,
    "wps": [
      {
        "id": "WP-01",
        "title": "Step with a hostile-looking brief",
        "tier": "haiku",
        "brief": "### WP-01\n- **DEPENDS-ON**: none\n- **GOAL**: brief text containing `)()`, `];process.exit(1)`, `${'x'}` and an unbalanced `[` inside a string"
      },
      {
        "id": "WP-02",
        "title": "Second step",
        "tier": "sonnet",
        "brief": "### WP-02\n- **DEPENDS-ON**: WP-01\n- **GOAL**: depends on WP-01"
      }
    ]
  }
]

globalThis.__IMPORTER_PWNED_BOTTOM__ = true
throw new Error('this file must never be executed')
