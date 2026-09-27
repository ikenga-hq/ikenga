// Title row slot (WP-20 skeleton, filled by WP-09). Exactly two controls on
// macOS — the project chip and the branch chip (spec §6A.4); WP-46 adds a
// third, the `≡` native-menu cascade button, on Windows/Linux. See
// ../title-row.tsx.
import { TitleRow } from '@/shell/title-row';

export function TitleRowSlot() {
	return <TitleRow />;
}
