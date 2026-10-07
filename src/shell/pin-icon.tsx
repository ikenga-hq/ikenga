// PinIcon: resolves a `{ iconLucide, iconEmoji }` pair into an icon node.
// Lucide name takes precedence (typed, sized to match the rail). Emoji is
// the fallback. If neither is set, falls back to a generic Folder/Pin
// glyph the caller picks via `fallback`.
//
// Lucide icon names are kebab-case (`LUCIDE_ICON_NAMES`; the glyphs themselves
// arrive from one lazily-loaded chunk, see `lib/icons/lucide-icons.ts`).
// Since WP-03 the rail no longer carries its own pkg icon whitelist
// (`PKG_ICONS`): a package's rail presence is a pin seeded from its manifest
// `ui.views[0].icon` (WP-22), and manifests spell those names either way
// (`layout-dashboard`, `LayoutDashboard`, `Box`). So the name is normalized
// to kebab-case and checked against that list; anything unknown renders
// the fallback instead of an empty button.

import type { LucideIcon } from 'lucide-react';
import type { IconName } from 'lucide-react/dynamic';
import { isLucideIconName } from '@/lib/icons/lucide-icons';
import { NamedLucideIcon } from '@/lib/icons/named-lucide-icon';

/** `LayoutDashboard` / `layoutDashboard` / `layout_dashboard` / ` Box ` →
 *  kebab-case, or null when the result is not a lucide icon name. Exported
 *  for tests. */
export function normalizeLucideName(name: string | null | undefined): IconName | null {
	if (!name) return null;
	const kebab = name
		.trim()
		.replace(/([a-z0-9])([A-Z])/g, '$1-$2')
		.replace(/([A-Z])([A-Z][a-z])/g, '$1-$2')
		.replace(/[\s_]+/g, '-')
		.toLowerCase();
	return isLucideIconName(kebab) ? (kebab as IconName) : null;
}

interface PinIconProps {
	iconLucide: string | null;
	iconEmoji: string | null;
	Fallback: LucideIcon;
	className?: string;
	/** Tailwind size class. Default `h-[18px] w-[18px]` (matches RailButton). */
	sizeClass?: string;
}

export function PinIcon({
	iconLucide,
	iconEmoji,
	Fallback,
	className,
	sizeClass = 'h-[18px] w-[18px]',
}: PinIconProps) {
	const lucide = normalizeLucideName(iconLucide);
	if (lucide) {
		return (
			<NamedLucideIcon
				name={lucide}
				Fallback={Fallback}
				className={`${sizeClass} ${className ?? ''}`}
			/>
		);
	}
	if (iconEmoji) {
		// Emoji size is roughly visual-equivalent at the same box; nudge with
		// leading-none so it centers in the same grid as a lucide glyph.
		return (
			<span
				aria-hidden="true"
				className={`${sizeClass} ${className ?? ''} grid place-items-center text-[15px] leading-none`}
			>
				{iconEmoji}
			</span>
		);
	}
	return <Fallback className={`${sizeClass} ${className ?? ''}`} />;
}
