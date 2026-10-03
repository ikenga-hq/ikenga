// Persistent banner shown when one or more installed pkgs require a
// connector that the user hasn't configured yet. Generalises the original
// SupabaseBanner — the rule is the same (banner appears iff at least one
// consumer pkg has an unconfigured connector), but driven by the
// connector registry + resolver so adding a new connector (Resend,
// Listmonk, Stripe, …) is purely additive.

import { useQuery, useQueryClient } from '@tanstack/react-query';
import { useNavigate } from '@tanstack/react-router';
import { AlertTriangle } from 'lucide-react';
import { useMemo } from 'react';

import { Banner } from '@/components/ui/banner';
import { Button } from '@/components/ui/button';
import { CONNECTOR_REGISTRY, type ConnectorId, findConnector } from '@/lib/onboarding/connectors';
import { resolveRequiredConnectors } from '@/lib/onboarding/resolve-connectors';
import { useInstalledManifests } from '@/lib/onboarding/use-installed-manifests';

const STATUS_QUERY_KEY = ['onboarding', 'connector-banner-status'] as const;

/**
 * Connector-aware workspace banner. Shows when at least one installed pkg
 * declares a connector that the host hasn't configured. Replaces
 * `SupabaseBanner` — drop-in: same import surface, broader rules.
 */
export function ConnectorBanner() {
	const navigate = useNavigate();
	const installed = useInstalledManifests();
	const queryClient = useQueryClient();

	const installedManifests = installed.data ?? [];
	const installedIds = useMemo(() => installedManifests.map((p) => p.pkgId), [installedManifests]);

	const requirements = useMemo(
		() =>
			resolveRequiredConnectors(
				installedIds,
				installedManifests.map((p) => p.manifest)
			),
		[installedIds, installedManifests]
	);

	const statuses = useQuery({
		queryKey: [...STATUS_QUERY_KEY, requirements.map((r) => r.connectorId).join(',')],
		queryFn: async () => {
			const out: Partial<Record<ConnectorId, boolean>> = {};
			for (const req of requirements) {
				const def = findConnector(req.connectorId);
				if (!def) continue;
				try {
					out[req.connectorId] = (await def.status()) === 'configured';
				} catch {
					out[req.connectorId] = false;
				}
			}
			return out;
		},
		staleTime: 60_000,
		refetchOnWindowFocus: false,
		enabled: requirements.length > 0,
	});

	if (installed.isLoading || installed.isError) return null;
	if (requirements.length === 0) return null;
	if (statuses.isLoading || statuses.isError) return null;

	const missing = requirements.filter((r) => statuses.data?.[r.connectorId] !== true);
	if (missing.length === 0) return null;

	const summary = describeMissing(missing.map((m) => m.connectorId));

	return (
		<Banner
			tone="warning"
			icon={<AlertTriangle />}
			data-testid="connector-banner"
			actions={
				<Button
					size="sm"
					onClick={() => {
						// Refresh the badge state when the user returns from settings.
						void queryClient.invalidateQueries({ queryKey: STATUS_QUERY_KEY });
						navigate({ to: '/settings/integrations' });
					}}
					data-testid="connector-banner-cta"
				>
					Open settings
				</Button>
			}
		>
			{summary}
		</Banner>
	);
}

/**
 * Back-compat alias — existing callers (`workspace.tsx`) imported
 * `SupabaseBanner`. New code should use `ConnectorBanner`. Kept here for
 * a deprecation window; the workspace re-import is switched in the same
 * commit so no breakage in practice.
 *
 * @deprecated Use {@link ConnectorBanner}.
 */
export const SupabaseBanner = ConnectorBanner;

// ── Pure helpers — exported for tests ──────────────────────────────────

/**
 * Build the user-facing copy for the banner. Single-connector phrasing is
 * specific (lists the connector); multi-connector phrasing is a count
 * because the list grows unbounded.
 */
export function describeMissing(ids: readonly ConnectorId[]): string {
	if (ids.length === 0) return '';
	if (ids.length === 1) {
		const def = CONNECTOR_REGISTRY.find((c) => c.id === ids[0]);
		const name = def?.display ?? ids[0];
		return `${name} isn't configured — installed apps and extensions that need it won't load data until you set it up.`;
	}
	return `${ids.length} connectors aren't configured — installed apps and extensions that need them won't load data until you set them up.`;
}
