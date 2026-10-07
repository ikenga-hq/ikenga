/** Whether an RPC error means the server doesn't run this command (the
 *  browser client), as opposed to the command itself failing. Matches the
 *  daemon's "not implemented in headless daemon" (`server/rpc.rs`). */
export function isUnavailableOnServer(msg: string): boolean {
	return (
		msg.includes('not implemented in headless daemon') ||
		msg.includes('not supported') ||
		msg.includes('unknown command')
	);
}

/** The phrase a daemon arm puts in a reason when it answers but cannot
 *  honestly evaluate something (no trust store, no sidecar supervisor, no
 *  install records — `server::shared::ngwa::NOT_AVAILABLE_ON_SERVER`). Such an
 *  answer reads "Not available on this server", never as an error, an empty
 *  list, a zero, "unsigned" or "healthy". */
export const NOT_AVAILABLE_ON_SERVER = 'not available on this server';

/** The UI copy for {@link NOT_AVAILABLE_ON_SERVER}. */
export const NOT_AVAILABLE_ON_SERVER_LABEL = 'Not available on this server';

/** Whether `msg` (an RPC error or a snapshot source's `error`) is the server
 *  saying it does not evaluate this at all. */
export function isNotAvailableOnServer(msg: string | null | undefined): boolean {
	return typeof msg === 'string' && msg.includes(NOT_AVAILABLE_ON_SERVER);
}
