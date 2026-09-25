#!/usr/bin/env bash
# Build this project's iPXE for every architecture the server hands out.
#
#   ipxe/build.sh [--arch=bios,x86_64-efi,i386-efi,arm64-efi]
#                 [--embed-base=http://10.0.0.2:8080 | --embed-base=none]
#                 [--embed-port=8080] [--dhcp-tries=3]
#                 [--engine=docker|podman] [--native] [--out=DIR]
#
# By default this runs itself inside a container (ipxe/Containerfile), because
# iPXE wants a Linux toolchain — gcc, binutils, an aarch64 cross compiler,
# liblzma for the BIOS compressor — and "install these seven packages first"
# is not a build anybody can reproduce. `--native` skips the container, for a
# Linux machine that already has them (and for the container build itself,
# which is how it runs this script).
#
# Outputs land in tftproot/ipxe-build/<arch>/, beside the loaders in use and
# never over them. Putting a build into service is a separate, deliberate
# step: `pxe:ipxe --install`, which keeps a backup.
#
# Environment, as an alternative to the flags:
#   PXE_EMBED_BASE   the server URL to bake in (empty: find it through DHCP)
#   IPXE_ENGINE      docker or podman
#   IPXE_MAKE_ARGS   extra arguments for iPXE's make, e.g. TRUST=… or DEBUG=…
#                    (paths in them are inside the container, where this
#                    directory is /work/ipxe)

set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/.." && pwd)"

all_arches="bios x86_64-efi i386-efi arm64-efi"

arches="$all_arches"
embed_base="${PXE_EMBED_BASE:-}"
embed_port="${PXE_EMBED_PORT:-8080}"
dhcp_tries="${PXE_DHCP_TRIES:-3}"
engine="${IPXE_ENGINE:-}"
native=0
out="${IPXE_OUT:-$repo/tftproot/ipxe-build}"
work="${IPXE_WORK:-$here/.work}"

die() {
    echo "ipxe/build.sh: $*" >&2
    exit 1
}

usage() {
    sed -n '2,26p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
}

# Accept the architecture names this project already uses elsewhere, so
# `--arch=x64-uefi` (the policy file's word) means the same as iPXE's.
normalise_arch() {
    case "$1" in
        bios | pcbios | i386-pcbios) echo bios ;;
        x86_64-efi | x64-uefi | x64 | x86_64 | efi64) echo x86_64-efi ;;
        i386-efi | x86-uefi | ia32 | efi32) echo i386-efi ;;
        arm64-efi | arm64-uefi | arm64 | aarch64) echo arm64-efi ;;
        *) die "unknown architecture \`$1\` (expected one of: $all_arches)" ;;
    esac
}

for argument in "$@"; do
    case "$argument" in
        --arch=*)
            list="${argument#--arch=}"
            arches=""
            for one in ${list//,/ }; do
                [ "$one" = all ] && { arches="$all_arches"; break; }
                arches="$arches $(normalise_arch "$one")"
            done
            ;;
        --embed-base=*) embed_base="${argument#--embed-base=}" ;;
        --embed-port=*) embed_port="${argument#--embed-port=}" ;;
        --dhcp-tries=*) dhcp_tries="${argument#--dhcp-tries=}" ;;
        --engine=*) engine="${argument#--engine=}" ;;
        --out=*) out="${argument#--out=}" ;;
        --native) native=1 ;;
        -h | --help) usage; exit 0 ;;
        *) die "unknown argument \`$argument\` (try --help)" ;;
    esac
done

arches="${arches# }" # the loop leaves a leading space
[ -n "$arches" ] || die "no architectures to build"

# `none` is how the Rust command says "bake nothing in" when .env has a base.
[ "$embed_base" = none ] && embed_base=""
embed_base="${embed_base%/}"
if [ -n "$embed_base" ]; then
    # A URL that iPXE's script parser would split, or that has a `$` it would
    # try to expand, is a build that boots nothing. Refuse it here.
    [[ "$embed_base" =~ ^https?://[^[:space:]\$#\&\|]+$ ]] ||
        die "--embed-base must be an http:// or https:// URL with no spaces, got \`$embed_base\`"
fi
[[ "$embed_port" =~ ^[0-9]{1,5}$ ]] || die "--embed-port must be a port number"
[[ "$dhcp_tries" =~ ^[1-9]$ ]] || die "--dhcp-tries must be between 1 and 9"

upstream_value() {
    sed -n "s/^$1=//p" "$here/UPSTREAM" | head -n 1
}
repository="$(upstream_value repository)"
commit="$(upstream_value commit)"
[[ "$commit" =~ ^[0-9a-f]{40}$ ]] || die "ipxe/UPSTREAM does not name a full commit hash"

# ---------------------------------------------------------------------------
# Host mode: run this script inside the container build (ipxe/Containerfile).
# ---------------------------------------------------------------------------
if [ "$native" -eq 0 ]; then
    if [ -z "$engine" ]; then
        for candidate in docker podman; do
            if command -v "$candidate" >/dev/null 2>&1 && "$candidate" info >/dev/null 2>&1; then
                engine="$candidate"
                break
            fi
        done
    fi
    if [ -z "$engine" ]; then
        installed=""
        for candidate in docker podman; do
            command -v "$candidate" >/dev/null 2>&1 && installed="$installed $candidate"
        done
        if [ -n "$installed" ]; then
            die "found${installed}, but no engine is running. Start Docker Desktop, or \`podman machine start\`, and try again. (Or --native on a Linux host with the toolchain.)"
        fi
        die "neither docker nor podman is installed. Install one, or run with --native on a Linux host with the toolchain from ipxe/Containerfile."
    fi

    # Git Bash hands docker `/c/Users/…`, which a Windows engine cannot read;
    # convert to `C:/Users/…` where that is what the host needs.
    host_path() {
        if command -v cygpath >/dev/null 2>&1; then cygpath -m "$1"; else printf '%s' "$1"; fi
    }

    mkdir -p "$out"
    out_on_host="$(host_path "$(cd "$out" && pwd)")"
    here_on_host="$(host_path "$here")"

    # A container *build*, with the binaries exported by the build itself,
    # rather than a `run` with the repository mounted: nothing has to be
    # shared into the engine's VM, nothing comes back owned by root, and it
    # works against a remote engine. amd64, because iPXE's x86 targets want
    # an x86 compiler; the engine emulates it on an arm64 host.
    exec "$engine" build --platform linux/amd64 \
        -f "$here_on_host/Containerfile" \
        --target output \
        --build-arg "ARCHES=${arches// /,}" \
        --build-arg "EMBED_BASE=${embed_base:-none}" \
        --build-arg "EMBED_PORT=$embed_port" \
        --build-arg "DHCP_TRIES=$dhcp_tries" \
        --build-arg "IPXE_MAKE_ARGS=${IPXE_MAKE_ARGS:-}" \
        --output "type=local,dest=$out_on_host" \
        "$here_on_host"
fi

# ---------------------------------------------------------------------------
# Native mode: fetch, configure, compile.
# ---------------------------------------------------------------------------
for tool in git make gcc perl sha256sum; do
    command -v "$tool" >/dev/null 2>&1 || die "\`$tool\` is not installed (see ipxe/Containerfile for the full list)"
done

mkdir -p "$work" "$out"

# The checkout is keyed on the commit and on the patches applied to it, so
# changing either gives a clean tree instead of patching a patched one.
patches=()
for patch in "$here"/patches/*.patch; do
    [ -e "$patch" ] && patches+=("$patch")
done
if [ "${#patches[@]}" -gt 0 ]; then
    patch_key="$(cat "${patches[@]}" | sha256sum | cut -c1-12)"
else
    patch_key="unpatched"
fi
src="$work/ipxe-${commit:0:12}-$patch_key"

if [ ! -f "$src/.kindling-ready" ]; then
    echo "==> fetching iPXE ${commit:0:12} from $repository"
    rm -rf "$src"
    git init -q "$src"
    git -C "$src" fetch -q --depth 1 "$repository" "$commit"
    git -C "$src" -c advice.detachedHead=false checkout -q FETCH_HEAD
    for patch in "${patches[@]}"; do
        echo "==> applying $(basename "$patch")"
        git -C "$src" apply "$patch"
    done
    touch "$src/.kindling-ready"
fi

# Our configuration replaces whatever local config the tree has, including
# headers left behind by an earlier run that this checkout no longer has.
find "$src/src/config/local" -maxdepth 1 -name '*.h' -delete
cp "$here"/config/local/*.h "$src/src/config/local/"

# The embedded script, with this build's answers filled in. `&` and `|` are
# special to sed; the URL was already refused if it had either, but escape
# them anyway so that validation and this line cannot drift apart.
embed="$work/embed.ipxe"
escaped_base="$(printf '%s' "$embed_base" | sed 's/[\\&|]/\\&/g')"
if [ -n "$embed_base" ]; then
    base_line="set sw-base $escaped_base"
else
    base_line="# no base URL baked in: found through DHCP below"
fi
sed -e "s|^set sw-base @PXE_EMBED_BASE@\$|$base_line|" \
    -e "s|@PXE_EMBED_PORT@|$embed_port|g" \
    -e "s|@PXE_DHCP_TRIES@|$dhcp_tries|g" \
    -e "s|@IPXE_COMMIT_SHORT@|${commit:0:12}|g" \
    "$here/embed.ipxe" | tr -d '\r' >"$embed"
if grep -q '@[A-Z_]*@' "$embed"; then
    die "embed.ipxe still has an unfilled @MARKER@ after substitution: $(grep -o '@[A-Z_]*@' "$embed" | head -n 1)"
fi

# Read back rather than repeated, so BUILD-INFO cannot disagree with what the
# binary actually sends.
user_class="$(sed -n 's/^set user-class //p' "$embed" | head -n 1)"
[ -n "$user_class" ] || die "embed.ipxe no longer sets a user-class"

host_machine="$(uname -m)"
jobs="$(nproc 2>/dev/null || echo 2)"
built_at="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
read -r -a extra_make <<<"${IPXE_MAKE_ARGS:-}"

for arch in $arches; do
    case "$arch" in
        bios) bin="bin-i386-pcbios"; targets=(undionly.kpxe); family=x86 ;;
        x86_64-efi) bin="bin-x86_64-efi"; targets=(ipxe.efi snponly.efi); family=x86 ;;
        i386-efi) bin="bin-i386-efi"; targets=(ipxe.efi snponly.efi); family=x86 ;;
        arm64-efi) bin="bin-arm64-efi"; targets=(ipxe.efi snponly.efi); family=arm64 ;;
    esac

    # Cross-compile whatever the host is not.
    cross=""
    case "$family:$host_machine" in
        x86:x86_64 | x86:i?86 | arm64:aarch64 | arm64:arm64) ;;
        x86:*) cross="x86_64-linux-gnu-" ;;
        arm64:*) cross="aarch64-linux-gnu-" ;;
    esac
    if [ -n "$cross" ] && ! command -v "${cross}gcc" >/dev/null 2>&1; then
        die "building $arch on $host_machine needs ${cross}gcc"
    fi

    make_targets=()
    for target in "${targets[@]}"; do
        make_targets+=("$bin/$target")
    done

    echo "==> building $arch: ${make_targets[*]}"
    # NO_WERROR: upstream compiles with -Werror, and a newer compiler than the
    # pinned commit was tested against grows new warnings. A warning should
    # not be the reason a boot loader cannot be rebuilt; it is still printed.
    make -C "$src/src" -j"$jobs" \
        ${cross:+CROSS="$cross"} \
        EMBED="$embed" \
        NO_WERROR=1 \
        "${extra_make[@]}" \
        "${make_targets[@]}"

    # Replace the output directory whole, so it never holds one new file and
    # one stale one with a BUILD-INFO that describes only half of them.
    staging="$out/.$arch.new"
    rm -rf "$staging"
    mkdir -p "$staging"
    for target in "${targets[@]}"; do
        cp "$src/src/$bin/$target" "$staging/$target"
    done
    (cd "$staging" && sha256sum "${targets[@]}" >SHA256SUMS)
    cat >"$staging/BUILD-INFO" <<EOF
# Written by ipxe/build.sh. Read by \`pxe:ipxe\` and \`pxe:doctor\`.
product=kindling iPXE
arch=$arch
files=${targets[*]}
repository=$repository
commit=$commit
patches=$patch_key
embed_base=$embed_base
embed_port=$embed_port
dhcp_tries=$dhcp_tries
user_class=$user_class
built_at=$built_at
EOF
    rm -rf "${out:?}/$arch"
    mv "$staging" "$out/$arch"

    for target in "${targets[@]}"; do
        printf '    %-26s %8d bytes\n' "$arch/$target" "$(wc -c <"$out/$arch/$target")"
    done
done

echo
if [ -n "$embed_base" ]; then
    echo "Built with $embed_base baked in, into $out."
else
    echo "Built with no server baked in (found through DHCP), into $out."
fi
echo "Nothing in service has changed. \`pxe:ipxe --install\` puts these in place."
