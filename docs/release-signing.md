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

Signing turns on when `APPLE_CERTIFICATE` is non-empty: `tauri build`
imports it into a throwaway keychain itself (decrypting with
`APPLE_CERTIFICATE_PASSWORD`) and signs with `APPLE_SIGNING_IDENTITY`.
Notarization follows automatically once either credential set exists — the
bundler notarizes the `.dmg` and staples the ticket. The release workflow
forwards each `APPLE_*` variable to the build only when the corresponding
secret is non-empty, because Tauri treats an *empty* variable as "set".

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

## Server artifacts

Every `v*` tag also publishes the headless multi-user server, built from the
same commit and carrying the same version as the desktop installers. The
release workflow (`release.yml`, jobs `server-build` and `server`) attaches:

| Asset | What it is |
|---|---|
| `ikenga-server_X.Y.Z_linux_amd64.tar.gz` | Binary, web app, systemd units, README, LICENSE for x86-64 |
| `ikenga-server_X.Y.Z_linux_arm64.tar.gz` | The same for aarch64 |
| `ikenga-server_X.Y.Z_manifest.json` | Schema `ikenga-server-release/1`: version, tag, commit, channel, glibc floor, and each tarball's SHA-256 and size |
| `<each of the three>.sigstore.json` | Keyless cosign bundle |

All of them are also covered by `SHA256SUMS.txt`, and each tarball and the
manifest carries a GitHub build-provenance attestation. If either server build
fails, the release stays a draft.

A tarball extracts straight into the install root (`/opt/ikenga`): `bin/`,
`dist/`, `systemd/`, `README.md`, `LICENSE`, `NOTICE` and a small
`release.json` naming the version, commit and architecture it was built from.

### glibc floor: 2.31

The binaries are dynamic glibc builds, not musl: the server looks up and
creates per-member Unix users, which needs NSS, and a static musl binary
cannot load NSS modules. `cargo-zigbuild` links against glibc **2.31**, so the
binary runs on Debian 11 and newer, Ubuntu 20.04 and newer, and RHEL, Rocky and
Alma 9 (glibc 2.34). The value lives in one place, `GLIBC_FLOOR` in
`scripts/server/package-server.sh`; the manifest records it as `glibc_floor`
and the release notes read it from there. To move the floor, change that one
default.

The build fails if the binary references a glibc symbol version above the
floor. To check an extracted binary yourself:

```bash
objdump -T bin/ikenga-server | grep -oE 'GLIBC_[0-9]+(\.[0-9]+)+' | sort -V | tail -1
```

### Binary size

SERVER_SIZE_PLACEHOLDER

### Verifying a server download

Download the tarball, its `.sigstore.json` bundle and `SHA256SUMS.txt`, then:

```bash
sha256sum --check --ignore-missing SHA256SUMS.txt

# The signature was made by the release workflow of a v* tag, nobody else.
cosign verify-blob \
  --bundle ikenga-server_X.Y.Z_linux_amd64.tar.gz.sigstore.json \
  --certificate-identity-regexp '^https://github.com/ikenga-hq/ikenga/.github/workflows/release.yml@refs/tags/v' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  ikenga-server_X.Y.Z_linux_amd64.tar.gz     # expect: Verified OK

gh attestation verify ikenga-server_X.Y.Z_linux_amd64.tar.gz -R ikenga-hq/ikenga
```

Signing is keyless: the signature is bound to the workflow run's identity, so
there is no key to store, rotate or leak and no secret to configure. It is
unrelated to the updater key below and to the registry's signing key.

### Testing without a tag

A manual run of the Release workflow (`workflow_dispatch`, platforms `all` or
`linux-only`) builds, signs and attests everything the same way and uploads the
files as a workflow artifact named `ikenga-server` instead of attaching them to
a release. The cosign certificate then names the branch the run used, not a
tag, so verify it with `--certificate-identity` set to that exact ref.

The CI workflow also builds both architectures and packs them, minus signing,
whenever `scripts/server/`, the build action or the workflow files change, and
on the full tier (release PRs and the nightly run), so a broken cross-build
shows up before a tag is cut.

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
