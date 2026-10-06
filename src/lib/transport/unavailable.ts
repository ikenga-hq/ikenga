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
