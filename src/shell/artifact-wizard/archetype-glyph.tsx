// An archetype's glyph, named by `Archetype.glyphName` (PascalCase, e.g.
// `LayoutDashboard`). Resolved through the shared lucide loader, never a
// `import * as Icons` namespace lookup: that kept every lucide icon in the
// boot bundle. Unknown names (and the moment before the icon chunk arrives)
// show `Square`, as an unknown name always has.

import { Square } from 'lucide-react';
import { NamedLucideIcon } from '@/lib/icons/named-lucide-icon';
import { normalizeLucideName } from '@/shell/pin-icon';

export function ArchetypeGlyph({ name, className }: { name: string; className?: string }) {
	const lucide = normalizeLucideName(name);
	if (!lucide) return <Square className={className} />;
	return <NamedLucideIcon name={lucide} Fallback={Square} className={className} />;
}
