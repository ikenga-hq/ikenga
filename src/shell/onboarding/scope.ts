// D-04 Personal / Project scope switch — shared by the two consecration steps
// where scope applies (`project`, `equipment`; design-spec-D-03-07 §D-04).
// One value across both steps, like the mock's single `S.scope`.
//
// `explicit` stays null until the user touches the switch: until then the
// effective scope is whatever the shipped steps already did implicitly —
// project when there is a project root to write under, personal otherwise
// (`rootSettingsEntry` in shell-store.ts) — so an untouched switch changes
// no write the old `roots` / `scaffolding` steps made. Session-local UI
// posture, not persisted.

import { create } from 'zustand';

import type { SettingsScopeId } from '@/shell/settings/nav';

interface OnboardingScopeState {
	explicit: SettingsScopeId | null;
	setScope: (scope: SettingsScopeId) => void;
}

export const useOnboardingScope = create<OnboardingScopeState>((set) => ({
	explicit: null,
	setScope: (explicit) => set({ explicit }),
}));

/** Project scope needs a root; without one the switch can only be personal. */
export function effectiveOnboardingScope(
	explicit: SettingsScopeId | null,
	projectRoot: string | null
): SettingsScopeId {
	if (!projectRoot) return 'personal';
	return explicit ?? 'project';
}
