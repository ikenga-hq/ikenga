// Side-effect module: the FIRST import in `main.tsx`, so it runs before any
// module that can reach the transport at load time (`home.ts` probing
// `fs_home`, `supabase.ts` awaiting `supabase_config_get`). Outside Tauri it
// marks this page as a browser tab served by the daemon, which makes
// `getTransport()` pick the HTTP transport from the first call instead of
// waiting for a token or the T1 tier probe. See `markBrowserEntry`.

import { capturePushDeepLink } from '@/lib/pwa/deeplink-capture';
import { isTauri, markBrowserEntry } from './index';

markBrowserEntry();

// plans/pwa S4 §7: a notification tap opens `?push=&ref=`. Take them out of
// the URL before boot can rewrite it (`/remote` on a phone); the app acts on
// them once it is up (`PushOpenHandler`).
if (!isTauri()) capturePushDeepLink();
