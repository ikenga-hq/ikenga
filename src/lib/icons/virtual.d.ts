// Virtual modules served by scripts/vite-plugin-lucide-icons.ts.

declare module 'virtual:lucide-icon-names' {
	/** Every kebab-case lucide icon name, aliases included. */
	const names: string[];
	export default names;
}

declare module 'virtual:lucide-icon-map' {
	import type { LucideIcon } from 'lucide-react';
	export function resolveIcon(
		name: string
	): LucideIcon | undefined | Promise<LucideIcon | undefined>;
}
