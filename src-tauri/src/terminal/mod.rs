// Moved to the ungated `server::shared::shell_detect` (WP-19 slice 5b) so the
// daemon's `terminal_detect_shells` arm detects with the same code.
pub use crate::server::shared::shell_detect;
