// "Permission requests" — D-05 `devices` segmented control (G-ACCESS §5.1,
// DEC-79). WP-74b ships this STUB; WP-75 fills it (`access_routing_get` /
// `access_routing_set`, the `this_device` device picker, `routing.changed`).
//
// Until then it renders the default (`any_approve`, D-05 "Any paired device")
// read-only, with the rule that already holds: a device can never approve
// beyond its own grant.

import { Segmented } from '@/components/ui/segmented';

export function RoutingPolicy() {
	return (
		<div data-routing="stub" className="flex min-w-0 flex-col gap-2">
			<Segmented
				ariaLabel="Who may answer Chi's asks"
				value="any_approve"
				onValueChange={() => {}}
				items={[
					{ id: 'this_device', label: 'This device only', disabled: true },
					{ id: 'any_approve', label: 'Any paired device' },
				]}
			/>
			<span className="rounded-md border border-dashed border-[var(--border)] px-3 py-1.5 text-[var(--text-micro)] text-[var(--fg-muted)]">
				Any device with <b className="text-[var(--fg)]">approve</b> may answer. A device can never
				approve more than its own capability allows. Choosing one device arrives with remote
				approvals.
			</span>
		</div>
	);
}
