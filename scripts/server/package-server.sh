#!/bin/bash
set -euo pipefail

# Build and package the headless server for a release.
#
#   package-server.sh build <amd64|arm64> [--expect-version X.Y.Z] [--out DIR]
#   package-server.sh pack  --version X.Y.Z --commit SHA --bin-dir DIR --dist DIR
#                           [--tag vX.Y.Z] [--out DIR] [--channel stable|next]
#                           [--min-upgrade-from X.Y.Z] [--published-at ISO8601]
#
# `build` cross-links `ikenga-server` with cargo-zigbuild against a PINNED
# glibc floor (not musl: the per-member user broker resolves users through
# NSS, which a static musl binary cannot load). It then gates the binary:
#   1. it links no GTK / WebKit (check-headless-link.sh),
#   2. the highest glibc symbol version it references is at or below the floor,
#   3. `ikenga-server --version` prints the expected version.
# Gates 1 and 3 execute the binary, so they run only when the host matches the
# requested architecture (CI builds each architecture on a native runner).
#
# `build` also builds `chi-runner` (the detached Chi-run helper) from the public
# ikenga-hq/iyke-cli repo at the pinned CHI_RUNNER_REF below, cross-linked the
# same way, and stages it as chi-runner-linux-<arch> beside the server binary.
# Its gates: glibc floor, ELF machine matches the architecture, and (on a
# matching host) a no-config run exits 2 with its "IKENGA_CHI_CONF not set"
# message. chi-runner has no --version or --help, so there is nothing else to
# ask it.
#
# `pack` turns the built binaries plus the web bundle into one tarball per
# architecture and writes the release manifest that lists them. Both
# ikenga-server and chi-runner are required: a missing chi-runner fails the
# pack instead of shipping a server that silently loses persistent Chi runs.
# It does not sign anything; the release workflow signs and attests what this
# produces.
#
# The glibc floor lives here and nowhere else. The manifest records it, the
# release notes read it from the manifest, and docs/release-signing.md quotes it.

# Oldest glibc a release binary will run on. 2.31 covers Debian 11 and
# RHEL/Rocky/Alma 9 (glibc 2.34) as well as Ubuntu 20.04 and newer.
GLIBC_FLOOR="${GLIBC_FLOOR:-2.31}"

# chi-runner lives in the public ikenga-hq/iyke-cli repo (standalone crate,
# [[bin]] "chi-runner"). It is built from this exact commit, never a branch or
# tag, so a release is reproducible and a force-push upstream cannot change what
# ships. To bump it: land the chi-runner change in iyke-cli, copy the full
# 40-character commit SHA of that commit here, and check that RunnerConf in
# iyke-cli/src/chi_runner.rs still matches RunnerConf in
# src-tauri/src/server/shared/chi_runner.rs (the server writes the conf the
# runner reads). CHI_RUNNER_REPO may point at a local clone for offline builds;
# the SHA is still the commit that gets built.
CHI_RUNNER_REF="dbf79f783c3b515aee29b1a2154abdb7c1dbb61b"
CHI_RUNNER_REPO="${CHI_RUNNER_REPO:-https://github.com/ikenga-hq/iyke-cli.git}"

# First release published with server tarballs (#381): the default
# min_upgrade_from for every later release.
FIRST_SERVER_RELEASE="0.19.4"

SHELL_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
MANIFEST_SCHEMA="ikenga-server-release/1"

die() { echo "error: $*" >&2; exit 1; }

usage() {
  sed -n '3,12p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//' >&2
  exit 2
}

# amd64 / arm64 -> Rust target triple and `uname -m` value.
triple_for() {
  case "$1" in
    amd64) echo "x86_64-unknown-linux-gnu" ;;
    arm64) echo "aarch64-unknown-linux-gnu" ;;
    *) die "unknown architecture '$1' (expected amd64 or arm64)" ;;
  esac
}
uname_for() {
  case "$1" in
    amd64) echo "x86_64" ;;
    arm64) echo "aarch64" ;;
  esac
}

# Version the server crate declares. Kept equal to the app version by
# scripts/sync-version.mjs, and what `ikenga-server --version` prints.
crate_version() {
  sed -n 's/^version = "\([^"]*\)".*/\1/p' "$SHELL_DIR/src-tauri/server/Cargo.toml" | head -n 1
}

# Highest `GLIBC_x.y` version a binary references, e.g. 2.31. Empty when it
# references none.
max_glibc_version() {
  local bin="$1" dump
  if command -v objdump >/dev/null 2>&1; then
    dump="$(objdump -T "$bin")"
  elif command -v readelf >/dev/null 2>&1; then
    dump="$(readelf --dyn-syms --wide "$bin")"
  else
    die "need objdump or readelf to check the glibc floor"
  fi
  grep -oE 'GLIBC_[0-9]+(\.[0-9]+)+' <<<"$dump" | sed 's/^GLIBC_//' | sort -V | tail -n 1 || true
}

# ELF machine of a binary, as `readelf -h` prints it ("Advanced Micro Devices
# X86-64" / "AArch64"); falls back to objdump's file format.
elf_machine() {
  local bin="$1"
  if command -v readelf >/dev/null 2>&1; then
    readelf -h "$bin" | sed -n 's/^ *Machine: *//p' | head -n 1
  elif command -v objdump >/dev/null 2>&1; then
    objdump -f "$bin" | sed -n 's/.*file format //p' | head -n 1
  else
    die "need readelf or objdump to check the binary's architecture"
  fi
}

# True when the ELF at $1 is built for architecture $2 (amd64|arm64).
elf_is_arch() {
  local m
  m="$(elf_machine "$1")"
  case "$2" in
    amd64) [[ "$m" == *"X86-64"* || "$m" == *x86-64* ]] ;;
    arm64) [[ "$m" == *AArch64* || "$m" == *aarch64* ]] ;;
    *) return 1 ;;
  esac
}

# True when version $1 is at or below version $2.
version_le() {
  [[ "$(printf '%s\n%s\n' "$1" "$2" | sort -V | head -n 1)" == "$1" ]]
}

summary() {
  if [[ -n "${GITHUB_STEP_SUMMARY:-}" ]]; then
    printf '%s\n' "$*" >> "$GITHUB_STEP_SUMMARY"
  fi
}

human_size() {
  local bytes="$1"
  awk -v b="$bytes" 'BEGIN { printf "%.1f MiB (%d bytes)", b / 1048576, b }'
}

# Build chi-runner for $1 (amd64|arm64) from iyke-cli at CHI_RUNNER_REF, gate it
# and stage it as $2/chi-runner-linux-$1. The source is fetched by full SHA into
# $2/.chi-runner-src (reused when it is already at the pin).
build_chi_runner() {
  local arch="$1" out_dir="$2" triple host_arch
  triple="$(triple_for "$arch")"
  host_arch="$(uname -m)"
  [[ "$CHI_RUNNER_REF" =~ ^[0-9a-f]{40}$ ]] || die "CHI_RUNNER_REF must be a full 40-character lowercase commit sha"

  local src="$out_dir/.chi-runner-src"
  mkdir -p "$out_dir"
  if [[ "$(git -C "$src" rev-parse HEAD 2>/dev/null || true)" != "$CHI_RUNNER_REF" ]]; then
    echo "==> Fetching chi-runner source $CHI_RUNNER_REPO @ $CHI_RUNNER_REF"
    rm -rf "$src"
    mkdir -p "$src"
    git -C "$src" init -q
    git -C "$src" fetch -q --depth 1 "$CHI_RUNNER_REPO" "$CHI_RUNNER_REF" \
      || die "could not fetch $CHI_RUNNER_REF from $CHI_RUNNER_REPO"
    git -C "$src" checkout -q --detach FETCH_HEAD
    [[ "$(git -C "$src" rev-parse HEAD)" == "$CHI_RUNNER_REF" ]] \
      || die "fetched chi-runner source is not at $CHI_RUNNER_REF"
  fi

  echo "==> Building chi-runner for $triple, glibc floor $GLIBC_FLOOR"
  # iyke-cli is a standalone crate with its own Cargo.lock: --locked builds
  # exactly the pinned dependency set. No link flags: it links no GTK/WebKit.
  (cd "$src" && cargo zigbuild --release --locked --bin chi-runner --target "$triple.$GLIBC_FLOOR")

  local bin="$src/target/$triple/release/chi-runner"
  [[ -f "$bin" ]] || die "build produced no $bin"

  elf_is_arch "$bin" "$arch" || die "$bin is not a $arch ELF ($(elf_machine "$bin"))"

  local highest
  highest="$(max_glibc_version "$bin")"
  echo "==> chi-runner highest glibc symbol version: ${highest:-none} (floor $GLIBC_FLOOR)"
  if [[ -n "$highest" ]] && ! version_le "$highest" "$GLIBC_FLOOR"; then
    die "$bin references GLIBC_$highest, above the pinned floor $GLIBC_FLOOR"
  fi

  if [[ "$host_arch" == "$(uname_for "$arch")" && "$(uname -s)" == "Linux" ]]; then
    echo "==> Verifying the binary links no desktop stack"
    "$SHELL_DIR/scripts/server/check-headless-link.sh" "$bin"
    chi_runner_smoke "$bin"
  else
    echo "warning: host is $host_arch, chi-runner is $arch; link and smoke gates skipped." >&2
  fi

  cp "$bin" "$out_dir/chi-runner-linux-$arch"
  local size
  size="$(wc -c < "$bin" | tr -d ' ')"
  echo "==> Staged $out_dir/chi-runner-linux-$arch, $(human_size "$size")"
  summary "- **chi-runner linux/$arch:** $(human_size "$size"), highest glibc symbol ${highest:-none}, floor $GLIBC_FLOOR, iyke-cli \`${CHI_RUNNER_REF:0:12}\`"
}

# chi-runner takes no arguments: it reads IKENGA_CHI_CONF and exits 2 with this
# message when it is unset. Running it that way proves the binary executes on
# this host and is the real chi-runner (a stray binary would not say it).
chi_runner_smoke() {
  local bin="$1" out rc=0
  out="$(env -u IKENGA_CHI_CONF "$bin" 2>&1 </dev/null)" || rc=$?
  if [[ $rc -ne 2 || "$out" != *"IKENGA_CHI_CONF not set"* ]]; then
    die "chi-runner smoke run: expected exit 2 with 'IKENGA_CHI_CONF not set', got exit $rc: $out"
  fi
  echo "==> chi-runner smoke run: exit 2, '$out'"
}

cmd_build() {
  local arch="${1:-}"
  [[ -n "$arch" ]] || usage
  shift
  local expect_version="" out_dir="$SHELL_DIR/scripts/server/out"
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --expect-version) expect_version="${2:?}"; expect_version="${expect_version#v}"; shift 2 ;;
      --out) out_dir="${2:?}"; shift 2 ;;
      *) die "unknown option '$1'" ;;
    esac
  done

  local triple host_arch
  triple="$(triple_for "$arch")"
  host_arch="$(uname -m)"
  [[ -n "$expect_version" ]] || expect_version="$(crate_version)"

  command -v cargo-zigbuild >/dev/null 2>&1 \
    || die "cargo-zigbuild is not installed (cargo install cargo-zigbuild, plus zig)"

  # The desktop library this daemon builds on is compiled as a cdylib too, so
  # cargo links the GTK/WebKit -sys crates even though the daemon itself
  # drops them (the link gate below proves it). zig does not search the
  # distro's library directory when the target carries a glibc version, so
  # point it there; and tolerate that those system libraries were built against
  # a newer glibc than the floor, which only matters for libraries the final
  # binary never loads.
  local libdir
  case "$arch" in
    amd64) libdir=/usr/lib/x86_64-linux-gnu ;;
    arm64) libdir=/usr/lib/aarch64-linux-gnu ;;
  esac
  local link_flags="-C link-arg=-L$libdir -C link-arg=-Wl,--allow-shlib-undefined"

  echo "==> Building ikenga-server for $triple, glibc floor $GLIBC_FLOOR"
  # The `.<floor>` suffix is cargo-zigbuild's glibc pin; cargo still writes to
  # target/<triple>/release.
  (cd "$SHELL_DIR/src-tauri"     && RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }$link_flags"        cargo zigbuild --release -p ikenga-server --target "$triple.$GLIBC_FLOOR")

  local bin="$SHELL_DIR/src-tauri/target/$triple/release/ikenga-server"
  [[ -f "$bin" ]] || die "build produced no $bin"

  # Gate 2: glibc floor. Reads the ELF only, so it works for either arch.
  local highest
  highest="$(max_glibc_version "$bin")"
  echo "==> Highest glibc symbol version referenced: ${highest:-none} (floor $GLIBC_FLOOR)"
  if [[ -n "$highest" ]] && ! version_le "$highest" "$GLIBC_FLOOR"; then
    die "$bin references GLIBC_$highest, above the pinned floor $GLIBC_FLOOR"
  fi

  if [[ "$host_arch" == "$(uname_for "$arch")" && "$(uname -s)" == "Linux" ]]; then
    # Gate 1: no desktop stack in the dynamic section.
    echo "==> Verifying the binary links no desktop stack"
    "$SHELL_DIR/scripts/server/check-headless-link.sh" "$bin"

    # Gate 3: the binary reports the version we expect.
    local reported
    reported="$("$bin" --version | awk '{print $NF}')"
    echo "==> ikenga-server --version: $reported (expected $expect_version)"
    [[ "$reported" == "$expect_version" ]] \
      || die "ikenga-server --version prints '$reported', expected '$expect_version'"
  else
    echo "warning: host is $host_arch, binary is $arch; link and --version gates skipped." >&2
  fi

  mkdir -p "$out_dir"
  cp "$bin" "$out_dir/ikenga-server-linux-$arch"
  local size
  size="$(wc -c < "$bin" | tr -d ' ')"
  echo "==> Staged $out_dir/ikenga-server-linux-$arch, $(human_size "$size")"
  summary "- **ikenga-server linux/$arch:** $(human_size "$size"), highest glibc symbol ${highest:-none}, floor $GLIBC_FLOOR"

  build_chi_runner "$arch" "$out_dir"
}

cmd_pack() {
  local version="" tag="" commit="" bin_dir="" dist_dir="" out_dir="$SHELL_DIR/scripts/server/out"
  local channel="" min_upgrade_from="" published_at=""
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --version) version="${2:?}"; shift 2 ;;
      --tag) tag="${2:?}"; shift 2 ;;
      --commit) commit="${2:?}"; shift 2 ;;
      --bin-dir) bin_dir="${2:?}"; shift 2 ;;
      --dist) dist_dir="${2:?}"; shift 2 ;;
      --out) out_dir="${2:?}"; shift 2 ;;
      --channel) channel="${2:?}"; shift 2 ;;
      --min-upgrade-from) min_upgrade_from="${2:?}"; shift 2 ;;
      --published-at) published_at="${2:?}"; shift 2 ;;
      *) die "unknown option '$1'" ;;
    esac
  done
  [[ -n "$version" && -n "$commit" && -n "$bin_dir" && -n "$dist_dir" ]] || usage
  [[ -d "$dist_dir" ]] || die "web bundle $dist_dir not found"
  [[ -f "$dist_dir/index.html" ]] || die "$dist_dir has no index.html; is it the Vite build output?"
  [[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.+-]+)?$ ]] || die "'$version' is not a semver version"
  [[ -n "$tag" ]] || tag="v$version"
  [[ "$tag" == "v$version" ]] || die "tag '$tag' does not match version '$version'"
  [[ "$commit" =~ ^[0-9a-f]{40}$ ]] || die "--commit must be a 40-character lowercase hex sha"
  [[ -n "$channel" ]] || { [[ "$version" == *-* ]] && channel=next || channel=stable; }
  # The oldest release a server can upgrade from directly. Defaulting this to
  # the release's own version made every published manifest refuse every
  # upgrade (found upgrading a live box to v0.20.1). Default to the first
  # release that shipped server tarballs; a release with a breaking migration
  # raises it explicitly with --min-upgrade-from.
  [[ -n "$min_upgrade_from" ]] || min_upgrade_from="$FIRST_SERVER_RELEASE"
  [[ "$min_upgrade_from" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "--min-upgrade-from must look like X.Y.Z"
  [[ -n "$published_at" ]] || published_at="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

  mkdir -p "$out_dir"
  out_dir="$(cd "$out_dir" && pwd)"
  local stage_root
  stage_root="$(mktemp -d)"
  trap 'rm -rf "$stage_root"' RETURN

  # Reproducible archives: sorted names, fixed owner, mtime pinned to the
  # commit, no gzip header timestamp.
  local epoch
  epoch="$(git -C "$SHELL_DIR" show -s --format=%ct "$commit" 2>/dev/null || true)"
  [[ -n "$epoch" ]] || epoch="$(date +%s)"

  local arch
  local manifest_artifacts=""
  for arch in amd64 arm64; do
    local bin="$bin_dir/ikenga-server-linux-$arch"
    [[ -f "$bin" ]] || die "missing $bin (run '$0 build $arch' first)"
    local chi_bin="$bin_dir/chi-runner-linux-$arch"
    [[ -f "$chi_bin" ]] || die "missing $chi_bin (run '$0 build $arch' first); a tarball without chi-runner would make persistent Chi runs fall back to in-process"
    elf_is_arch "$chi_bin" "$arch" || die "$chi_bin is not a $arch ELF ($(elf_machine "$chi_bin"))"

    # Run the amd64 binary when this host can: the tarball must carry a binary
    # of the version the tag names. arm64 was checked on its own runner.
    if [[ "$arch" == "amd64" && "$(uname -m)" == "x86_64" && "$(uname -s)" == "Linux" ]]; then
      chmod +x "$bin"
      local reported
      reported="$("$bin" --version | awk '{print $NF}')"
      [[ "$reported" == "$version" ]] \
        || die "amd64 ikenga-server --version prints '$reported', expected '$version'"
    fi

    local highest
    highest="$(max_glibc_version "$bin")"
    if [[ -n "$highest" ]] && ! version_le "$highest" "$GLIBC_FLOOR"; then
      die "$arch binary references GLIBC_$highest, above the pinned floor $GLIBC_FLOOR"
    fi

    local chi_highest
    chi_highest="$(max_glibc_version "$chi_bin")"
    if [[ -n "$chi_highest" ]] && ! version_le "$chi_highest" "$GLIBC_FLOOR"; then
      die "$arch chi-runner references GLIBC_$chi_highest, above the pinned floor $GLIBC_FLOOR"
    fi
    if [[ "$arch" == "amd64" && "$(uname -m)" == "x86_64" && "$(uname -s)" == "Linux" ]]; then
      chmod +x "$chi_bin"
      chi_runner_smoke "$chi_bin"
    fi

    local stage="$stage_root/$arch"
    mkdir -p "$stage/bin"
    install -m 0755 "$bin" "$stage/bin/ikenga-server"
    install -m 0755 "$chi_bin" "$stage/bin/chi-runner"
    cp -r "$dist_dir" "$stage/dist"
    find "$stage/dist" -type d -exec chmod 0755 {} +
    find "$stage/dist" -type f -exec chmod 0644 {} +
    local unit
    for unit in "$SHELL_DIR"/scripts/server/*.service; do
      install -m 0644 "$unit" "$stage/$(basename "$unit")"
    done
    install -m 0644 "$SHELL_DIR/scripts/server/README.md" "$stage/README.md"
    install -m 0644 "$SHELL_DIR/LICENSE" "$stage/LICENSE"
    local members=(bin dist README.md LICENSE release.json)
    if [[ -f "$SHELL_DIR/NOTICE" ]]; then
      install -m 0644 "$SHELL_DIR/NOTICE" "$stage/NOTICE"
      members+=(NOTICE)
    fi

    # Build record for this one tarball. It carries no digests: the release
    # manifest holds those, and a file cannot hold its own archive's hash.
    ARCH="$arch" VERSION="$version" TAG="$tag" COMMIT="$commit" FLOOR="$GLIBC_FLOOR" CHI_REF="$CHI_RUNNER_REF" \
      node -e '
        const e = process.env;
        const rec = { schema: "ikenga-server-build/1", version: e.VERSION, tag: e.TAG,
                      commit: e.COMMIT, arch: e.ARCH, glibc_floor: e.FLOOR,
                      chi_runner_ref: e.CHI_REF };
        process.stdout.write(JSON.stringify(rec, null, 2) + "\n");
      ' > "$stage/release.json"
    chmod 0644 "$stage/release.json"

    local name="ikenga-server_${version}_linux_${arch}.tar.gz"
    echo "==> Packing $name"
    (
      cd "$stage"
      # GNU tar flags; the release runner is Linux.
      tar --sort=name --owner=0 --group=0 --numeric-owner --mtime="@$epoch" \
        -cf - bin dist ikenga-server*.service README.md LICENSE release.json $([[ -f NOTICE ]] && echo NOTICE) \
        | gzip -n -9 > "$out_dir/$name"
    )

    # Sanity: the members an installer depends on are really in the archive.
    local listing
    listing="$(tar -tzf "$out_dir/$name")"
    local member
    for member in bin/ikenga-server bin/chi-runner dist/index.html README.md LICENSE release.json; do
      grep -qx "$member" <<<"$listing" || die "$name is missing $member"
    done
    grep -Eq '^ikenga-server.*\.service$' <<<"$listing" || die "$name carries no systemd unit"

    local sha size
    sha="$(sha256sum "$out_dir/$name" | awk '{print $1}')"
    size="$(wc -c < "$out_dir/$name" | tr -d ' ')"
    local bin_size
    bin_size="$(wc -c < "$bin" | tr -d ' ')"
    echo "    $sha  $(human_size "$size"); binary $(human_size "$bin_size")"
    summary "- **$name:** $(human_size "$size"), binary $(human_size "$bin_size"), sha256 \`$sha\`"
    manifest_artifacts+="$arch|$name|$sha|$size"$'\n'
  done

  local manifest="$out_dir/ikenga-server_${version}_manifest.json"
  VERSION="$version" TAG="$tag" COMMIT="$commit" PUBLISHED_AT="$published_at" CHANNEL="$channel" \
    FLOOR="$GLIBC_FLOOR" MIN_FROM="$min_upgrade_from" SCHEMA="$MANIFEST_SCHEMA" ROWS="$manifest_artifacts" \
    node -e '
      const e = process.env;
      const artifacts = e.ROWS.split("\n").filter(Boolean).map((row) => {
        const [arch, name, sha256, size] = row.split("|");
        return { kind: "tarball", arch, name, sha256, size: Number(size),
                 sigstore_bundle: name + ".sigstore.json" };
      });
      const manifest = {
        schema: e.SCHEMA, version: e.VERSION, tag: e.TAG, commit: e.COMMIT,
        published_at: e.PUBLISHED_AT, channel: e.CHANNEL, glibc_floor: e.FLOOR,
        min_upgrade_from: e.MIN_FROM || null, migrations: "forward-only", artifacts,
      };
      process.stdout.write(JSON.stringify(manifest, null, 2) + "\n");
    ' > "$manifest"
  echo "==> Wrote $manifest"
  summary "- **Manifest:** \`$(basename "$manifest")\`, glibc floor $GLIBC_FLOOR, channel $channel"
}

case "${1:-}" in
  build) shift; cmd_build "$@" ;;
  pack) shift; cmd_pack "$@" ;;
  *) usage ;;
esac
