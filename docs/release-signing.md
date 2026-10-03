# Release signing

How Ikenga releases get code-signed, which secrets power it, and how to
verify a signed build.

The short version: **signing is off today and turns on automatically the
moment the secrets exist.** Nothing in `tauri.conf.json` needs to change,
and contributors' local builds are unaffected — they keep producing unsigned
bundles either way.

Two different kinds of signing are involved:

- **Updater signing** (`TAURI_SIGNING_PRIVATE_KEY`) — already active. Signs
  the `*.sig` files the in-app updater verifies. This is *not* OS code
  signing; it's a Minisign keypair owned by the project.
- **OS code signing** — off today, opt-in per platform. macOS uses an Apple
  "Developer ID Application" certificate plus notarization; Windows uses
  Azure Artifact Signing (formerly Azure Trusted Signing / Code Signing).

## Secrets inventory

Where things live: **Settings → Secrets and variables → Actions** on
`ikenga-hq/ikenga`. Credentials go under the *Secrets* tab; non-sensitive
configuration under the *Variables* tab.

### Already configured (do not create)

| Secret | What it is | Used for |
|---|---|---|
| `TAURI_SIGNING_PRIVATE_KEY` | Minisign private key | Signs `.sig` updater artifacts on every leg (already active) |

### macOS — create these to turn signing on

| Secret / variable | Where it comes from |
|---|---|
| `APPLE_CERTIFICATE` | Base64 of a **Developer ID Application** certificate exported from Keychain Access as `.p12`: `openssl base64 -A -in cert.p12`. Requires a paid Apple Developer account; only the Account Holder can create Developer ID certs. |
| `APPLE_CERTIFICATE_PASSWORD` | Password chosen when exporting the `.p12`. |
| `APPLE_SIGNING_IDENTITY` | The identity string `security find-identity -v -p codesigning` prints, e.g. `Developer ID Application: Your Name (TEAMID)`. |
| `APPLE_API_KEY` + `APPLE_API_ISSUER` + `APPLE_API_KEY_P8` | *Preferred notarization auth.* From App Store Connect → Users and Access → Integrations: `APPLE_API_KEY` is the Key ID, `APPLE_API_ISSUER` the Issuer ID, `APPLE_API_KEY_P8` the **contents** of the `AuthKey_<id>.p8` file (downloadable once). The workflow writes it to disk and points `APPLE_API_KEY_PATH` at it. |
| `APPLE_ID` + `APPLE_PASSWORD` + `APPLE_TEAM_ID` | *Alternative notarization auth* — used if the API key isn't set. `APPLE_PASSWORD` is an app-specific password (appleid.apple.com), not the account password. `APPLE_TEAM_ID` is on the Apple Developer membership page. |

Signing turns on when `APPLE_CERTIFICATE` is non-empty: the release workflow
imports it into a throwaway keychain and `tauri build` picks up
`APPLE_SIGNING_IDENTITY`. Notarization follows automatically once either
credential set exists — the bundler notarizes the `.dmg` and staples the
ticket.

### Windows — create these to turn signing on

| Secret / variable | Kind | Where it comes from |
|---|---|---|
| `AZURE_TENANT_ID` | Secret | Microsoft Entra tenant ID (Azure portal → Entra ID → Overview). |
| `AZURE_CLIENT_ID` + `AZURE_CLIENT_SECRET` | Secret | An App Registration granted the *Artifact Signing Certificate Profile Signer* role on the Artifact Signing account. |
| `AZURE_ARTIFACT_SIGNING_ENDPOINT` | Variable | Region endpoint of the Artifact Signing account, e.g. `https://wus2.codesigning.azure.net`. |
| `AZURE_ARTIFACT_SIGNING_ACCOUNT` | Variable | Name of the Artifact Signing account in Azure. |
| `AZURE_ARTIFACT_SIGNING_CERT_PROFILE` | Variable | Name of the certificate profile inside that account. |

All six must be non-empty for signing to engage; if any is missing the leg
logs "building unsigned" and produces exactly the artifacts it does today.
The workflow compiles `artifact-signing-cli` on the runner and injects
`bundle.windows.signCommand` through `tauri build --config` — deliberately
**not** committed to `tauri.conf.json`, so a contributor's plain
`tauri build` never tries to reach Azure.

### Linux

Nothing. There is no mainstream code-signing story for `.deb` / `.AppImage`;
`SHA256SUMS.txt` (below) is the tamper check.

## `SHA256SUMS.txt`

Every GitHub release gets a `SHA256SUMS.txt` asset covering every installer
and updater artifact. Verify a download with:

```bash
sha256sum --check --ignore-missing SHA256SUMS.txt
```

from a directory containing the sums file plus the assets you fetched.

## The updater key: how it's held and how to rotate

`TAURI_SIGNING_PRIVATE_KEY` is a repository Actions secret on
`ikenga-hq/ikenga`. The matching public key is committed in
`src-tauri/tauri.conf.json` at `plugins.updater.pubkey`, which means it is
**compiled into every shipped binary** — that has consequences for rotation
(see below). The workflow passes the private key straight through to
`tauri-action`; a `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` secret is supported by
Tauri if the key was generated with one (ours isn't).

To rotate:

1. Generate a new keypair: `bunx tauri signer generate` (writes
   `~/.tauri/ikenga.key` or prints to stdout).
2. Replace the `TAURI_SIGNING_PRIVATE_KEY` secret value.
3. Replace `plugins.updater.pubkey` in `tauri.conf.json` with the new public
   key.
4. Cut a release.

**Caveat:** an installed app verifies updates against the pubkey baked into
*its* binary. After rotation, every release built with the new key fails
verification on clients that predate the pubkey swap — they must reinstall
manually. The least-bad path is to rotate immediately after a release when
most active clients already run it, and call the breakage out in the release
notes. There is no dual-signature support upstream.

## Verifying a signed build

macOS (against the `.dmg` or the installed `.app`):

```bash
codesign --verify --deep --strict /Applications/Ikenga.app
spctl -a -vv /Applications/Ikenga.app          # expect: accepted, source=Notarized Developer ID
xcrun stapler validate Ikenga_*.dmg            # expect: "The validate action worked!"
```

Windows (PowerShell, against the setup exe):

```powershell
Get-AuthenticodeSignature .\Ikenga_*_x64-setup.exe | Format-List
# expect: Status = Valid, SignerCertificate Subject = Ikenga
```

Linux: `sha256sum --check` against `SHA256SUMS.txt`, and `.sig` files are
verified automatically by the in-app updater.

## What changes for users

- **macOS:** the "Apple can't check it for malicious software" /
  "unidentified developer" dialogs disappear — Gatekeeper accepts the
  notarized build on first launch. No more right-click → Open workaround.
- **Windows:** SmartScreen's "Windows protected your PC" interstitial goes
  away. Azure Artifact Signing carries Microsoft's own reputation, so this
  is immediate rather than reputation-earned.
- **Auto-update:** unchanged — the updater already verifies the Minisign
  `.sig` regardless of OS signing.
- **Linux:** unchanged.
