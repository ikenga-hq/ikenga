// WP-20 (frame slot skeleton): title row.
//
// No title-row component exists yet in the current frame — the design's
// planned title row (P2, `designs/frame-workbench-v4.html`) has not been
// built. This slot renders null until a later WP (per the design's title
// row region) introduces one. Kept as its own file so that WP lands here
// without re-entering `workspace.tsx`.
export function TitleRowSlot() {
	return null;
}
