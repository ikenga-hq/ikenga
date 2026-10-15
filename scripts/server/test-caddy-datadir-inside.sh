#!/bin/bash
# Run as root inside test-caddy-datadir-container.sh's container.
set -euo pipefail

T="$(mktemp -d)"
PROVISION=/work/provision.sh
SHIM=$T/shim; mkdir -p "$SHIM"
CALLS=$T/calls
LIST=/etc/apt/sources.list.d/caddy-stable.list
KEYRING=/usr/share/keyrings/caddy-stable-archive-keyring.gpg
TMPROOT=$T/tmp; mkdir -p "$TMPROOT"
CHECKS=0

fail() { echo "FAIL: $*" >&2; [[ -f "${OUT:-}" ]] && { echo "--- last output ---" >&2; tail -20 "$OUT" >&2; echo "--- calls ---" >&2; cat "$CALLS" >&2 2>/dev/null; }; exit 1; }
ok() { local d="$1"; shift; CHECKS=$((CHECKS + 1)); "$@" || fail "$d"; }
no() { local d="$1"; shift; CHECKS=$((CHECKS + 1)); if "$@"; then fail "$d"; fi; }
calls_have() { grep -qE -- "$1" "$CALLS"; }
out_has() { grep -qE -- "$1" "$OUT"; }

# ---- extract the code under test from provision.sh (no edits, no test hooks)
{
  grep -E '^(log|note|die)\(\)' "$PROVISION"
  awk '/^changed\(\) \{/,/^\}/; /^run\(\) \{/,/^\}/' "$PROVISION"
  grep -E '^CADDY_DEB_(VERSION|SHA512_AMD64|SHA512_ARM64)=' "$PROVISION"
  for f in caddy_from_repo caddy_from_deb install_caddy ensure_t1_data_dir; do
    awk -v f="$f" '$0 ~ "^"f"\\(\\) \\{" {p=1} p {print} p && /^\}/ {exit}' "$PROVISION"
  done
} > "$T/fns.sh"
grep -q '^caddy_from_deb()' "$T/fns.sh" && grep -q '^ensure_t1_data_dir()' "$T/fns.sh" || fail "could not extract functions from provision.sh"

# ---- shims
cat > "$SHIM/curl" <<'SH'
#!/bin/bash
echo "curl $*" >> "$CALLS"
url="" out=""
while [[ $# -gt 0 ]]; do case "$1" in -o) out="$2"; shift 2 ;; -*) shift ;; *) url="$1"; shift ;; esac; done
case "$url" in
  *cloudsmith*gpg.key)  [[ "${SHIM_REPO:-ok}" == key ]] && exit 22; echo KEY ;;
  *cloudsmith*deb.txt)  [[ "${SHIM_REPO:-ok}" == list ]] && exit 22; echo "deb https://example/caddy" > "$out" ;;
  *github.com/caddyserver/caddy/releases/*.deb) [[ -n "${SHIM_DEB_FAIL:-}" ]] && exit 22; cp "$SHIM_DEB" "$out" ;;
  *) exit 6 ;;
esac
SH
cat > "$SHIM/gpg" <<'SH'
#!/bin/bash
echo "gpg $*" >> "$CALLS"
out=""; while [[ $# -gt 0 ]]; do [[ "$1" == -o ]] && out="$2"; shift; done
cat > "$out"
SH
cat > "$SHIM/apt-get" <<'SH'
#!/bin/bash
echo "apt-get[DEBIAN_FRONTEND=${DEBIAN_FRONTEND:-unset}] $*" >> "$CALLS"
case "$1" in
  update)  # a stale/broken caddy list makes update fail, as on 2026-10-13
    [[ "${SHIM_REPO:-ok}" == update && -f /etc/apt/sources.list.d/caddy-stable.list ]] && exit 100
    [[ -f /etc/apt/sources.list.d/caddy-stable.list && -n "${SHIM_STALE_BREAKS:-}" ]] && exit 100
    exit 0 ;;
  install) [[ " $* " == *" caddy "* && "${SHIM_REPO:-ok}" == install ]] && exit 100; exit 0 ;;
esac
SH
chmod +x "$SHIM"/*
mkdir -p /etc/apt/sources.list.d /usr/share/keyrings

# A fake .deb, and its sha512 (the "pin" for the success cases only: production
# pins are never touched, the wrong-pin cases run against them as shipped).
printf 'not really a deb\n' > "$T/fake.deb"
FAKE_SHA="$(sha512sum "$T/fake.deb" | cut -d' ' -f1)"

RC=0
# scenario <env assignments...> -- <shell snippet>: runs install_caddy in a subshell
scenario() {
  : > "$CALLS"; rm -f "$LIST" "$KEYRING"; rm -rf "$TMPROOT"; mkdir -p "$TMPROOT"
  OUT="$T/out"
  local envs=() snippet
  while [[ "$1" != -- ]]; do envs+=("$1"); shift; done; shift; snippet="$1"
  if env PATH="$SHIM:/usr/bin:/bin" CALLS="$CALLS" TMPDIR="$TMPROOT" SHIM_DEB="$T/fake.deb" "${envs[@]}" \
      bash -c 'set -euo pipefail; source "$1"; DRY_RUN="${DRY_RUN:-0}"; CADDY_DEB_FALLBACK="${CADDY_DEB_FALLBACK:-1}"; CHANGES=(); '"$snippet"'; printf "CHANGES: %s\n" "${CHANGES[@]:-}"' _ "$T/fns.sh" > "$OUT" 2>&1; then RC=0; else RC=$?; fi
}
left_tmp() { [[ -n "$(ls -A "$TMPROOT")" ]]; }

# 1. repo works: no .deb, source list kept
scenario ARCH=amd64 -- 'install_caddy'
ok "repo ok: exit 0" test "$RC" -eq 0
ok "repo ok: apt-get install caddy ran non-interactively" calls_have 'apt-get\[DEBIAN_FRONTEND=noninteractive\] install -y --no-install-recommends caddy'
no "repo ok: no .deb download" calls_have 'github.com'
ok "repo ok: list kept" test -f "$LIST"
ok "repo ok: reported" out_has 'caddy installed from the Cloudsmith apt repo'

# 2-4. repo fails at each step -> pinned .deb (pin overridden to the fake .deb's hash)
for step in key list update install; do
  scenario ARCH=amd64 SHIM_REPO=$step -- "CADDY_DEB_SHA512_AMD64=$FAKE_SHA; install_caddy"
  ok "repo fails at $step: exit 0 via fallback" test "$RC" -eq 0
  ok "repo fails at $step: .deb installed by path with deps (apt-get install)" calls_have 'apt-get\[DEBIAN_FRONTEND=noninteractive\] install -y /.*/caddy_2\.11\.7_linux_amd64\.deb'
  ok "repo fails at $step: URL is the pinned release" calls_have 'github.com/caddyserver/caddy/releases/download/v2\.11\.7/caddy_2\.11\.7_linux_amd64\.deb'
  no "repo fails at $step: source list removed" test -e "$LIST"
  no "repo fails at $step: keyring removed" test -e "$KEYRING"
  no "repo fails at $step: temp dir cleaned" left_tmp
  ok "repo fails at $step: path reported" out_has 'installed from the pinned release \.deb'
done

# 5. wrong pin (production pin vs the fake .deb): dies, nothing installed, cleaned up
scenario ARCH=amd64 SHIM_REPO=list -- 'install_caddy'
ok "wrong pin: non-zero exit" test "$RC" -ne 0
ok "wrong pin: sha512 mismatch named" out_has 'sha512 mismatch'
ok "wrong pin: both causes named" out_has 'apt repo: could not fetch the apt source list\. Pinned \.deb: sha512 mismatch'
no "wrong pin: .deb never handed to apt" calls_have 'install -y /.*\.deb'
no "wrong pin: source list removed" test -e "$LIST"
no "wrong pin: temp dir cleaned" left_tmp

# 6. download fails
scenario ARCH=amd64 SHIM_REPO=key SHIM_DEB_FAIL=1 -- 'install_caddy'
ok "download fails: non-zero exit" test "$RC" -ne 0
ok "download fails: both causes named" out_has 'apt repo: could not fetch the signing key\. Pinned \.deb: could not download caddy_2\.11\.7_linux_amd64\.deb'
no "download fails: temp dir cleaned" left_tmp

# 7. arm64 picks the arm64 .deb and its own pin; unknown arch dies clearly
scenario ARCH=arm64 SHIM_REPO=key -- "CADDY_DEB_SHA512_ARM64=$FAKE_SHA; install_caddy"
ok "arm64: exit 0" test "$RC" -eq 0
ok "arm64: arm64 URL" calls_have 'caddy_2\.11\.7_linux_arm64\.deb'
scenario ARCH=riscv64 SHIM_REPO=key -- 'install_caddy'
ok "riscv64: dies" test "$RC" -ne 0
ok "riscv64: says which arch" out_has "no pinned Caddy \.deb for architecture 'riscv64'"
no "riscv64: nothing downloaded" calls_have 'github.com'

# 8. CADDY_DEB_FALLBACK=0: no fallback, but the stale list is still removed
scenario ARCH=amd64 SHIM_REPO=update CADDY_DEB_FALLBACK=0 -- 'install_caddy'
ok "fallback off: dies" test "$RC" -ne 0
ok "fallback off: names the cause" out_has 'apt-get update failed\) and CADDY_DEB_FALLBACK=0'
no "fallback off: no .deb download" calls_have 'github.com'
no "fallback off: source list removed" test -e "$LIST"

# 9. dry run: nothing fetched, nothing run, nothing written
scenario ARCH=amd64 DRY_RUN=1 -- 'install_caddy'
ok "dry-run: exit 0" test "$RC" -eq 0
ok "dry-run: no curl/gpg/apt calls" test ! -s "$CALLS"
no "dry-run: no list" test -e "$LIST"
ok "dry-run: says what it would do" out_has '\[dry-run\] if that fails: remove its source list, install the pinned v2\.11\.7'
ok "dry-run: change prefixed would:" out_has 'CHANGES: would: caddy installed'

# 10. caddy already present: untouched
printf '#!/bin/sh\n' > "$SHIM/caddy"; chmod +x "$SHIM/caddy"
scenario ARCH=amd64 -- 'install_caddy'
ok "present: exit 0, no calls" test "$RC" -eq 0 -a ! -s "$CALLS"
rm -f "$SHIM/caddy"

# ---- T1 data dir
D=$T/ikenga; mkdir -p "$D"
dd_scenario() {   # <tier> <dry> -- uses $D as INSTALL_DIR
  OUT="$T/out"
  if env INSTALL_DIR="$D" TIER="$1" DRY_RUN="$2" bash -c 'set -euo pipefail; source "$1"; CHANGES=(); ensure_t1_data_dir; printf "CHANGES: %s\n" "${CHANGES[@]:-}"' _ "$T/fns.sh" > "$OUT" 2>&1; then RC=0; else RC=$?; fi
}
mode() { stat -c '%a %U:%G' "$1"; }

rm -rf "$D/data"; mkdir -p "$D/data/principals" "$D/data/operator"
chmod 0700 "$D/data"; chmod 0711 "$D/data/principals"; chmod 0700 "$D/data/operator"
echo secret > "$D/data/operator/key"; chmod 0600 "$D/data/operator/key"
inside_before="$(stat -c '%a %U:%G %n' "$D"/data/principals "$D"/data/operator "$D"/data/operator/key)"

dd_scenario t1 1
ok "dry-run t1: data untouched (700)" test "$(mode "$D/data")" = "700 root:root"
ok "dry-run t1: says what it would do" out_has '\[dry-run\] chmod 0711'
ok "dry-run t1: change prefixed would:" out_has 'CHANGES: would: data dir'

dd_scenario t0 0
ok "t0: stays 700" test "$(mode "$D/data")" = "700 root:root"
ok "t0: no change reported" out_has '^CHANGES: $'

dd_scenario t1 0
ok "t1: pre-existing 0700 converges to 0711 root:root" test "$(mode "$D/data")" = "711 root:root"
ok "t1: change reported" out_has 'CHANGES: data dir .*: 700 root:root -> 711 root:root'
ok "t1: nothing inside data/ touched" test "$(stat -c '%a %U:%G %n' "$D"/data/principals "$D"/data/operator "$D"/data/operator/key)" = "$inside_before"

dd_scenario t1 0
ok "t1: second run is a no-op" out_has '^CHANGES: $'

chown 1000:1000 "$D/data"; chmod 0755 "$D/data"
dd_scenario t1 0
ok "t1: wrong owner/mode (1000:1000 755) converges" test "$(mode "$D/data")" = "711 root:root"

rm -rf "$D/data"
dd_scenario t1 1
no "dry-run t1, dir missing: not created" test -e "$D/data"
dd_scenario t1 0
ok "t1, dir missing: created 0711 root:root" test "$(mode "$D/data")" = "711 root:root"

# install_daemon's own mkdir: the t1/t0 split is a one-liner we can only grep for.
CHECKS=$((CHECKS + 1))
grep -qE 'if \[\[ "\$TIER" == t1 \]\]; then install -d -m 0711 -o root -g root "\$INSTALL_DIR/data"; else install -d -m 0700 "\$INSTALL_DIR/data"; fi' "$PROVISION" \
  || fail "install_daemon data-dir line not as expected"

echo "==> [Container] $CHECKS checks PASSED"
