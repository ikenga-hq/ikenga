// Dev-only globals. Imported eagerly by `main.tsx` in dev builds so iyke
// (and developer console scripting) can reach helpers without going
// through a UI. Production builds tree-shake the import via the
// `import.meta.env.DEV` guard in main.tsx.

// Side-effect import: installs window.__bgSpikeReply + window.bgSpikeRun.
import './bg-spike';
// Side-effect import: installs window.__windowCostPing + window.windowCostRun.
// Required in every window that loads the dev bundle so Rust's eval() poll
// can fire the first-paint signal in thin/full probe windows (WP-01).
import './window-cost';

export {};
