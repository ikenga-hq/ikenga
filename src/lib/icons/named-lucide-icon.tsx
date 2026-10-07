// A lucide icon picked by (kebab-case) name at runtime. Renders `Fallback`
// until the shared icon chunk delivers the glyph, and for good when the name
// is unknown, so a rail button or action row never goes empty.

import type { LucideIcon, LucideProps } from 'lucide-react';
import { useLucideIcon } from './lucide-icons';

export interface NamedLucideIconProps extends LucideProps {
	name: string;
	Fallback: LucideIcon;
}

export function NamedLucideIcon({ name, Fallback, ...props }: NamedLucideIconProps) {
	const Icon = useLucideIcon(name) ?? Fallback;
	return <Icon {...props} />;
}
