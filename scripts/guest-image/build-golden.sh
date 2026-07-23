#!/bin/sh
# build-golden.sh — produce bough's VM-sandbox golden guest rootfs DIRECTORY.
#
# Output: an unpacked Alpine rootfs dir carrying the full agent toolchain
# (git, deno, nodejs/npm, python3, build-base/gcc, socat, openssl, coreutils),
# a CA baked into the system trust store, and `git safe.directory = *`.
# The dir is consumed later as `smolvm machine create --image <dir>/ ...`.
#
# Design constraints honoured:
#   - NO docker, NO registry push. Base image is pulled by a throwaway smolvm
#     microVM (which needs egress -> we give it open --net just for the build).
#   - Golden delivery is a DIRECTORY, never a .smolmachine pack. The pack is only
#     an intermediate we crack open to get the single flattened OCI layer.
#   - The `smolvm pack --from`/registry path is known to silently drop egress
#     flags, so we never ship a pack; we only mine its flattened layer tar.
#   - Idempotent: prior output is cleaned; the temp machine name is unique per run.
#
# Usage:
#   build-golden.sh [--ca /abs/path/to/ca.crt] [--out /abs/output/dir]
#
#   --ca   CA cert (PEM) to install into the guest trust store. This is the seam
#          where bough's real Claw Patrol CA gets injected. Default: generate a
#          throwaway self-signed CA under the work dir.
#   --out  Output rootfs directory. Default: <script dir>/golden-rootfs
set -eu

# ---------------------------------------------------------------------------
# Config / args
# ---------------------------------------------------------------------------
SMOLVM="${BOUGH_SMOLVM_BIN:-smolvm}"
BASE_IMAGE="${BASE_IMAGE:-alpine:3.22}"
# Intermediates (pack, extraction stage, throwaway CA) go to a temp dir — never the
# script's own directory, so a repo checkout stays clean. Removed by cleanup().
WF="$(mktemp -d "${TMPDIR:-/tmp}/bough-golden.XXXXXX")"

CA_CERT=""                       # empty => generate a throwaway CA
OUT_DIR="$WF/golden-rootfs"

while [ $# -gt 0 ]; do
  case "$1" in
    --ca)  CA_CERT="$2"; shift 2 ;;
    --out) OUT_DIR="$2"; shift 2 ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done

# Unique temp machine name (idempotent re-runs never collide) and intermediates.
MACHINE="wf1-build-$$-$(date +%s)"
PACK="$WF/golden.smolmachine"           # pack stub + <PACK>.smolmachine sidecar
SIDECAR="$PACK.smolmachine"
STAGE="$WF/pack-extract"                # where we crack the sidecar open

log() { printf '\n=== %s ===\n' "$*"; }

# Best-effort cleanup of the temp machine + intermediates on any exit path.
cleanup() {
  "$SMOLVM" machine delete --name "$MACHINE" -f >/dev/null 2>&1 || true
  rm -rf "$WF" 2>/dev/null || true
}
trap cleanup EXIT INT TERM

# ---------------------------------------------------------------------------
# 0. Idempotency: wipe prior output + intermediates
# ---------------------------------------------------------------------------
log "clean prior output"
rm -rf "$OUT_DIR" "$STAGE" "$PACK" "$SIDECAR"
mkdir -p "$OUT_DIR" "$STAGE"

# ---------------------------------------------------------------------------
# 1. Throwaway build machine on the base image, WITH egress (apk needs it)
# ---------------------------------------------------------------------------
log "create + start build machine $MACHINE on $BASE_IMAGE"
"$SMOLVM" machine create --name "$MACHINE" --net --image "$BASE_IMAGE"
"$SMOLVM" machine start  --name "$MACHINE"

# ---------------------------------------------------------------------------
# 2. Install the agent toolchain.
#    Deno IS in Alpine v3.22 community, so no special handling is required.
#    If any package is unavailable on some future base, apk fails loudly here.
# ---------------------------------------------------------------------------
log "apk add toolchain"
"$SMOLVM" machine exec --name "$MACHINE" -- sh -c '
  set -e
  apk update
  apk add --no-cache \
    git deno nodejs npm python3 build-base socat \
    ca-certificates openssl coreutils
'

# ---------------------------------------------------------------------------
# 3. Install the CA into the guest system trust store.
#    Prefer bough's real per-install Claw Patrol CA (so every guest trusts the live
#    MITM proxy natively — no per-session delivery). Fall back to a throwaway.
# ---------------------------------------------------------------------------
BOUGH_CA="${BOUGH_HOME:-$HOME/.bough}/net/ca/ca.crt"
if [ -z "$CA_CERT" ] && [ -f "$BOUGH_CA" ]; then
  CA_CERT="$BOUGH_CA"
  log "using bough's Claw Patrol CA -> $CA_CERT"
elif [ -z "$CA_CERT" ]; then
  CA_CERT="$WF/throwaway-ca.crt"
  log "no bough CA at $BOUGH_CA — generating throwaway -> $CA_CERT"
  openssl req -x509 -newkey rsa:2048 -nodes \
    -keyout "$WF/throwaway-ca.key" -out "$CA_CERT" -days 3650 \
    -subj "/CN=bough-clawpatrol-golden-throwaway-CA" 2>/dev/null
fi
log "install CA into guest trust store"
"$SMOLVM" machine cp "$CA_CERT" "$MACHINE:/usr/local/share/ca-certificates/clawpatrol-ca.crt"
"$SMOLVM" machine exec --name "$MACHINE" -- update-ca-certificates

# ---------------------------------------------------------------------------
# 4. git safe.directory '*' — worktrees will be host-user-owned inside the VM,
#    so git must not refuse to operate on them.
# ---------------------------------------------------------------------------
log "git config --system safe.directory '*'"
"$SMOLVM" machine exec --name "$MACHINE" -- git config --system safe.directory '*'

# ---------------------------------------------------------------------------
# 4b. Pre-create the workspace mount point. A local-dir image's lower layer is
#     read-only, so smolvm can't create /workspace itself at --volume time
#     (fails "Read-only file system"); the mount target must exist in the rootfs.
# ---------------------------------------------------------------------------
log "pre-create /workspace mount point"
"$SMOLVM" machine exec --name "$MACHINE" -- mkdir -p /workspace

# ---------------------------------------------------------------------------
# 4c. Bake bough's CA-rewritten kubeconfig (if present) at the guest path the
#     gateway points KUBECONFIG at in VM mode. kubectl's EKS token is stamped by
#     the host proxy — the guest carries no cluster credential, only this config.
# ---------------------------------------------------------------------------
BOUGH_KUBECONFIG="${BOUGH_HOME:-$HOME/.bough}/net/kubeconfig"
if [ -f "$BOUGH_KUBECONFIG" ]; then
  log "bake kubeconfig -> /etc/bough/kubeconfig"
  "$SMOLVM" machine exec --name "$MACHINE" -- mkdir -p /etc/bough
  "$SMOLVM" machine cp "$BOUGH_KUBECONFIG" "$MACHINE:/etc/bough/kubeconfig"
fi

# ---------------------------------------------------------------------------
# 4d. Extra guest tools from the user's ~/.bough/guest-tools (seeded by
#     `bough update`; edit it to add tools to the sandbox). One entry per line:
#       pkg <name>        apk package (Alpine community)
#       bin <name> <url>  linux/arm64 binary, fetched host-side -> /usr/local/bin
# ---------------------------------------------------------------------------
TOOLS_FILE="${BOUGH_HOME:-$HOME/.bough}/guest-tools"
BIN_NAMES=""
if [ -f "$TOOLS_FILE" ]; then
  log "extra guest tools from $TOOLS_FILE"
  PKGS=""
  while IFS= read -r line || [ -n "$line" ]; do
    line="${line%%#*}"
    # shellcheck disable=SC2086
    set -- $line
    [ $# -eq 0 ] && continue
    case "$1" in
      pkg)
        [ $# -eq 2 ] || { echo "bad pkg line in $TOOLS_FILE: $*" >&2; exit 2; }
        PKGS="$PKGS $2"
        ;;
      bin)
        [ $# -eq 3 ] || { echo "bad bin line in $TOOLS_FILE: $*" >&2; exit 2; }
        echo "fetch $2 <- $3"
        curl -fsSL -o "$WF/tool-$2" "$3"
        "$SMOLVM" machine cp "$WF/tool-$2" "$MACHINE:/usr/local/bin/$2"
        "$SMOLVM" machine exec --name "$MACHINE" -- chmod 755 "/usr/local/bin/$2"
        BIN_NAMES="$BIN_NAMES $2"
        ;;
      *)
        echo "unknown directive in $TOOLS_FILE (want 'pkg NAME' or 'bin NAME URL'): $*" >&2
        exit 2
        ;;
    esac
  done < "$TOOLS_FILE"
  if [ -n "$PKGS" ]; then
    echo "apk add:$PKGS"
    "$SMOLVM" machine exec --name "$MACHINE" -- sh -c "apk add --no-cache$PKGS"
  fi
fi

# ---------------------------------------------------------------------------
# 5. Flatten + extract the machine's rootfs into OUT_DIR.
#    Method: pack the (stopped) VM to a .smolmachine, whose sidecar is a
#    zstd(tar) holding a single flattened OCI layer `layers/<sha>.tar`
#    (plus agent-rootfs.tar and a 20GiB sparse storage.ext4 we must NOT read).
#    We stream-decompress and extract ONLY the layer member with bsdtar -q
#    (fast-read: stops after the last matched member, before storage.ext4),
#    then untar that layer into OUT_DIR.
# ---------------------------------------------------------------------------
log "stop machine + pack --from-vm"
"$SMOLVM" machine stop --name "$MACHINE"
"$SMOLVM" pack create --from-vm "$MACHINE" -o "$PACK" --no-sign

log "extract flattened OCI layer from sidecar"
# Discover the layer member name (stream-list, head closes the pipe early).
LAYER_MEMBER="$(zstd -dc "$SIDECAR" 2>/dev/null | tar tf - 2>/dev/null \
                 | grep -m1 '^layers/.*\.tar$')"
[ -n "$LAYER_MEMBER" ] || { echo "FATAL: no layer member in sidecar" >&2; exit 1; }
echo "layer member: $LAYER_MEMBER"
# -q stops reading after this member, so the 20GiB storage.ext4 is never touched.
zstd -dc "$SIDECAR" 2>/dev/null | tar xqf - -C "$STAGE" "$LAYER_MEMBER"

LAYER_TAR="$STAGE/$LAYER_MEMBER"

# --- Assert the layer is the full toolchain, not a stale tiny base layer. ---
# The pack path is flaky; a correct flattened layer is ~350-460MB. Guard on size.
LAYER_BYTES="$(wc -c < "$LAYER_TAR" | tr -d ' ')"
MIN_BYTES=$((300 * 1024 * 1024))     # 300MB floor
echo "flattened layer size: $LAYER_BYTES bytes"
if [ "$LAYER_BYTES" -lt "$MIN_BYTES" ]; then
  echo "FATAL: layer $LAYER_BYTES bytes < ${MIN_BYTES} floor — stale/tiny layer, aborting." >&2
  exit 1
fi

log "unpack layer -> $OUT_DIR"
tar xf "$LAYER_TAR" -C "$OUT_DIR"

# --- Post-extract sanity: the toolchain binaries must be present on disk. ---
log "verify baked contents"
MISSING=""
for b in usr/bin/git usr/bin/deno usr/bin/node usr/bin/npm \
         usr/bin/python3 usr/bin/gcc usr/bin/socat usr/bin/openssl; do
  if [ -e "$OUT_DIR/$b" ]; then echo "OK   $b"; else echo "MISS $b"; MISSING="$MISSING $b"; fi
done
for b in $BIN_NAMES; do
  if [ -e "$OUT_DIR/usr/local/bin/$b" ]; then echo "OK   usr/local/bin/$b"; else echo "MISS usr/local/bin/$b"; MISSING="$MISSING $b"; fi
done
[ -e "$OUT_DIR/usr/local/share/ca-certificates/clawpatrol-ca.crt" ] \
  && echo "OK   CA baked" || { echo "MISS CA"; MISSING="$MISSING ca"; }
grep -q 'directory = \*' "$OUT_DIR/etc/gitconfig" 2>/dev/null \
  && echo "OK   git safe.directory" || { echo "MISS git safe.directory"; MISSING="$MISSING gitconfig"; }
[ -z "$MISSING" ] || { echo "FATAL: missing:$MISSING" >&2; exit 1; }

echo "golden rootfs size: $(du -sh "$OUT_DIR" | cut -f1)"

# ---------------------------------------------------------------------------
# 6. Delete the temp machine (also covered by the EXIT trap).
# ---------------------------------------------------------------------------
log "delete build machine"
"$SMOLVM" machine delete --name "$MACHINE" -f
rm -rf "$WF" 2>/dev/null || true
trap - EXIT INT TERM

log "DONE"
echo "golden rootfs: $OUT_DIR"
