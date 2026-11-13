#!/bin/sh
# Mindspace v0.1.0-alpha.1 release installer.
#
# This installs the qualified, source-only Alpha (RC.5 archive) from its GitHub release. It is NOT a
# build script for the code on the main branch: a clone of this repository, its tag and GitHub's
# generated "Source code" archives all contain an older implementation. This script never uses them.
#
# What it does: checks your system, authenticates the release (pinned SHA-256 values and detached
# signatures from a publisher key you confirm), builds the archive with Rust 1.98.1 in a private
# temporary directory and installs one binary under
#   $HOME/.local/lib/mindspace/0.1.0-alpha.1/bin/mind
# What it never does: use sudo, edit shell startup files, touch an agent configuration or an existing
# database, replace an existing install without --reinstall, or fall back to an unverified download.
#
# Usage:  sh install.sh [--trust-fingerprint FPR] [--verify-only] [--assets-dir DIR]
#                       [--key-file FILE] [--upgrade] [--reinstall] [--allow-older] [--keep-work] [--help]

set -eu
umask 077
export LC_ALL=C

VERSION=0.1.0-alpha.1
TAG=v$VERSION
BASE_URL=https://github.com/VikingOwl91/mindspace/releases/download/$TAG
TOPDIR=mindspace-$VERSION
ARCHIVE=mindspace-$VERSION-rc.5-source.tar.gz
MANIFEST=source-archive-manifest.json
ARCHIVE_SHA=b587a04edff66963162267fff805678b7c1ba8aefbaec795e518702e0e4149c0
MANIFEST_COMMIT=a027d94680392648b6277dc2fd9bc20906114b91
RUST_VERSION=1.98.1
NEED_KIB=2000000

# Publisher signing key (primary fingerprint). The same value is in the public documentation.
FPR=0E6699CB48F3941AE28CA88D509F78007F62D5EA
FPR_SPACED='0E66 99CB 48F3 941A E28C  A88D 509F 7800 7F62 D5EA'
WKD_URL='https://openpgpkey.nachtigall.dev/.well-known/openpgpkey/nachtigall.dev/hu/rckmfa7rw857sf15hkixzhxx4681turz?l=christian'

# sha256, size in bytes, name of every release asset. All seven are required.
ASSET_TABLE='b587a04edff66963162267fff805678b7c1ba8aefbaec795e518702e0e4149c0 1384547 mindspace-0.1.0-alpha.1-rc.5-source.tar.gz
6604a649c6795249ebf168116418825db9bef08ef370b2a1884aee4df190b685 228 mindspace-0.1.0-alpha.1-rc.5-source.tar.gz.asc
903d8fd226593b74083bc8c0a11b7ffafc520eae06b5f054d0565ad1f5dae391 58659 source-archive-manifest.json
f2eda80092b572761e80f381128e75afee4b042a21af66d5a1064d9aabf52f2d 228 source-archive-manifest.json.asc
5fc56e89bed615a7755bfe4e28eb63bd3fe6e855f586bae6142c14754ecb06ef 204 SHA256SUMS
86c667876ad6e91a7dce4341c44d4ea2207fdba7c3ad368d51f74dfdc2a46b84 6280 VERIFY.txt
bcba2140c52fb84befaedba8ae1dfd8b1aa2fb121149a2d6bba5e4736e88879b 680 mindspace-release-signing-key.asc'

WORK=
PREFIX=
STAGED=
BACKUP_HOLDER=
INSTALL_OK=0
BUILD_PID=
KEEP_WORK=0
VERIFY_ONLY=0
ASSETS_DIR=
KEY_FILE=
TRUST_FPR=
UPGRADE=0
REINSTALL=0
ALLOW_OLDER=0
MOVED_OLD=0
BACKUP=
STATE=none
REASON=
REPAIRABLE=1

say() { printf '%s\n' "$*"; }
step() { printf '\n==> %s\n' "$*"; }
die() { printf 'STOP: %s\n' "$*" >&2; exit 1; }

usage() {
    cat <<'EOF'
Mindspace v0.1.0-alpha.1 release installer (Linux x86_64, source-only Alpha)

  sh install.sh [options]

Options:
  --trust-fingerprint FPR  Your confirmed publisher key fingerprint (40 hex characters). Required when
                           the installer is not run in a terminal. Without it you are asked to type it.
  --verify-only            Download and authenticate the release assets, then stop. Nothing is built or
                           installed.
  --assets-dir DIR         Use the seven release assets you already downloaded into DIR instead of
                           downloading them. They are verified exactly like downloaded ones.
  --key-file FILE          Use this public key file, which you obtained independently, instead of
                           fetching the publisher key from the publisher's domain.
  --upgrade                Install pinned RC.5 beside validated older versions; no discovery or activation.
  --reinstall              Replace an existing installation of this version. Nothing is replaced
                           until a fresh build has succeeded; the old one is moved to
                           ~/.local/lib/mindspace/previous/, not deleted. Failure before replacement
                           restores it if the original path remains free.
  --allow-older            Install this version although a newer one is already installed. It is
                           installed beside the newer one; your PATH decides which one runs.
  --keep-work              Keep the temporary working directory (for inspection) when finished.
  -h, --help               Show this text.

Run twice, it does nothing the second time: an installation that is present and intact is reported
as "Already up to date" and left untouched.

The installer needs: curl, gpg (GnuPG with --assert-signer), tar, gzip, sha256sum, a C compiler and
Rust 1.98.1. See docs/install.md in the repository for the trust decision and troubleshooting.
EOF
}

cleanup() {
    rc=$?
    trap - EXIT INT TERM HUP
    if [ -n "$BUILD_PID" ]; then
        # The build runs in its own session and process group (its id is recorded by the build
        # wrapper): stop cargo, rustc, build scripts and compilers, not just the first process.
        pg=
        [ -r "$WORK/build.pgid" ] && pg=$(cat "$WORK/build.pgid" 2>/dev/null)
        case "$pg" in '' | *[!0-9]*) pg=$BUILD_PID ;; esac
        # Never signal group 0 or 1 (every process of the user), whatever the file contained.
        [ "$pg" -gt 1 ] 2>/dev/null || pg=$BUILD_PID
        kill -s TERM -- "-$pg" 2>/dev/null || true
        kill -s TERM "$BUILD_PID" 2>/dev/null || true
        sleep 1
        kill -s KILL -- "-$pg" 2>/dev/null || true
        wait "$BUILD_PID" 2>/dev/null || true
    fi
    if [ -n "$WORK" ] && [ -d "$WORK/gnupg" ] && command -v gpgconf >/dev/null 2>&1; then
        gpgconf --homedir "$WORK/gnupg" --kill all >/dev/null 2>&1 || true
    fi
    if [ -n "$STAGED" ] && [ -d "$STAGED" ]; then
        rm -f "$STAGED/bin/mind" "$STAGED/install-record" 2>/dev/null || true
        rmdir "$STAGED/bin" "$STAGED" 2>/dev/null || true
    fi
    if [ "$MOVED_OLD" = 1 ] && [ "$INSTALL_OK" != 1 ] && [ -n "$BACKUP" ] && { [ -e "$BACKUP" ] || [ -L "$BACKUP" ]; }; then
        if ! [ -e "$PREFIX" ] && ! [ -L "$PREFIX" ] && mv -T -- "$BACKUP" "$PREFIX" 2>/dev/null; then
            say "The previous installation was restored at $PREFIX." >&2
        else
            say "The previous installation is kept at $BACKUP. Inspect it before restoring it to $PREFIX." >&2
        fi
    fi
    [ -z "$BACKUP_HOLDER" ] || rmdir "$BACKUP_HOLDER" 2>/dev/null || true
    if [ -n "$WORK" ] && [ -d "$WORK" ]; then
        if [ "$KEEP_WORK" = 1 ]; then
            say "Working directory kept: $WORK"
        else
            case "$WORK" in
                */mindspace-install.*)
                    rm -rf -- "$WORK" 2>/dev/null || { sleep 1; rm -rf -- "$WORK" 2>/dev/null; } ||
                        say "Could not remove the temporary directory $WORK; you can delete it." >&2
                    ;;
            esac
        fi
    fi
    exit "$rc"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP

# ---------------------------------------------------------------- arguments
while [ $# -gt 0 ]; do
    case "$1" in
        --trust-fingerprint)
            [ $# -ge 2 ] || die "--trust-fingerprint needs a value"
            TRUST_FPR=$2
            shift 2
            ;;
        --assets-dir)
            [ $# -ge 2 ] || die "--assets-dir needs a directory"
            ASSETS_DIR=$2
            shift 2
            ;;
        --key-file)
            [ $# -ge 2 ] || die "--key-file needs a file"
            KEY_FILE=$2
            shift 2
            ;;
        --verify-only) VERIFY_ONLY=1; shift ;;
        --upgrade) UPGRADE=1; shift ;;
        --reinstall) REINSTALL=1; shift ;;
        --allow-older) ALLOW_OLDER=1; shift ;;
        --keep-work) KEEP_WORK=1; shift ;;
        -h | --help) usage; exit 0 ;;
        *) usage >&2; die "unknown option: $1" ;;
    esac
done

# ---------------------------------------------------------------- 1. this system
step "Checking this system"

[ "$(uname -s 2>/dev/null)" = Linux ] || die "this Alpha is qualified for Linux only (found $(uname -s 2>/dev/null || echo unknown))."
[ "$(uname -m 2>/dev/null)" = x86_64 ] || die "this Alpha is qualified for x86_64 only (found $(uname -m 2>/dev/null || echo unknown))."

case "${HOME:-}" in
    /*) ;;
    *) die "HOME must be set to an absolute path." ;;
esac
if ! [ -d "$HOME" ] || ! [ -w "$HOME" ]; then die "HOME ($HOME) is not a writable directory."; fi
INSTALL_ROOT=$HOME/.local/lib/mindspace
PREFIX=$INSTALL_ROOT/$VERSION

MISSING=
need_cmd() { command -v "$1" >/dev/null 2>&1 || MISSING="$MISSING $1"; }
for c in sha256sum awk grep tr cut mv date timeout; do need_cmd "$c"; done
if [ -n "$MISSING" ]; then
    die "missing required tools:$MISSING. Install them with your system's package manager, then run this again."
fi

# ---------------------------------------------------------------- 1b. an existing installation
# Compare two versions (MAJOR.MINOR.PATCH with an optional -prerelease, as in semantic versioning).
# Prints -1, 0 or 1. A prerelease is lower than the release; numeric identifiers compare as numbers.
ver_cmp() {
    awk -v a="$1" -v b="$2" '
    # Compare unbounded decimal strings without floating point rounding.
    function numcmp(a, b) {
        sub(/^0+/, "", a); sub(/^0+/, "", b)
        if (length(a) != length(b)) return length(a) < length(b) ? -1 : 1
        return "x" a == "x" b ? 0 : ("x" a < "x" b ? -1 : 1)
    }
    function parse(v, o,   i, core) {
        sub(/\+.*/, "", v)
        i = index(v, "-")
        o["has"] = i ? 1 : 0; o["pre"] = i ? substr(v, i + 1) : ""
        split(i ? substr(v, 1, i - 1) : v, core, ".")
        o[1] = core[1]; o[2] = core[2]; o[3] = core[3]
    }
    function cmp(a, b,   x, y, i, c, n, pa, pb, na, nb, ia, ib, ra, rb) {
        parse(a, x); parse(b, y)
        for (i = 1; i <= 3; i++) {c = numcmp(x[i], y[i]); if (c) return c}
        if (!x["has"] && !y["has"]) return 0
        if (!x["has"]) return 1
        if (!y["has"]) return -1
        na = split(x["pre"], pa, "."); nb = split(y["pre"], pb, ".")
        n = na < nb ? na : nb
        for (i = 1; i <= n; i++) {
            ia = pa[i]; ib = pb[i]
            ra = (ia ~ /^[0-9]+$/); rb = (ib ~ /^[0-9]+$/)
            if (ra && rb) {c = numcmp(ia, ib); if (c) return c}
            else if (ra) return -1
            else if (rb) return 1
            else if ("x" ia != "x" ib) return "x" ia < "x" ib ? -1 : 1
        }
        return na == nb ? 0 : (na < nb ? -1 : 1)
    }
    BEGIN {print cmp(a, b)}'
}

# Version names are SemVer, including hyphenated prerelease identifiers and build metadata.
is_version() {
    printf '%s\n' "$1" | grep -Eq '^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-([0-9A-Za-z-]+)(\.[0-9A-Za-z-]+)*)?(\+[0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*)?$' || return 1
    printf '%s\n' "$1" | awk '
        {sub(/\+.*/, ""); sub(/^[^-]*-?/, ""); n=split($0, a, ".")
         for (i=1; i<=n; i++) if (a[i] ~ /^0[0-9]+$/) exit 1}'
}

check_install_parents() {
    for parent in "$HOME/.local" "$HOME/.local/lib" "$INSTALL_ROOT"; do
        [ ! -L "$parent" ] || die "$parent is a symbolic link. Inspect this installation path before proceeding; nothing was replaced."
        if [ -e "$parent" ] && [ ! -d "$parent" ]; then
            die "$parent is not a directory. Inspect this installation path before proceeding."
        fi
    done
}

# Sets STATE to none, valid (intact, with an install record), legacy (runs and reports the right
# version but has no record) or invalid, and REASON for invalid. A directory that merely exists is
# never treated as an installation. The record is a consistency check against accidental damage; it
# is not protection against someone who can already write to your home directory.
inspect_existing() { # PREFIX EXPECTED_VERSION; never execute a binary with a mismatching receipt.
    iprefix=$1
    expected=$2
    STATE=none
    REASON=
    REPAIRABLE=1
    if ! [ -e "$iprefix" ] && ! [ -L "$iprefix" ]; then return 0; fi
    STATE=invalid
    if [ -L "$iprefix" ]; then REASON="it is a symbolic link"; return 0; fi
    if ! [ -d "$iprefix" ]; then REASON="it is not a directory"; return 0; fi
    ibin=$iprefix/bin/mind
    # Check for data before any repairable layout/binary failure returns.
    # Never follow a linked bin directory to perform this inspection.
    for directory in "$iprefix" "$iprefix/bin"; do
        if [ "$directory" != "$iprefix" ]; then
            [ -d "$directory" ] && [ ! -L "$directory" ] || continue
        fi
        for entry in "$directory"/* "$directory"/.[!.]* "$directory"/..?*; do
            [ -e "$entry" ] || [ -L "$entry" ] || continue
            case "$entry" in
                "$ibin" | "$iprefix/install-record")
                    if [ -d "$entry" ] && [ ! -L "$entry" ]; then
                        REASON="unexpected directory $entry; inspect and back up its contents before repair"
                        REPAIRABLE=0
                        return 0
                    fi
                    continue ;;
                "$iprefix/bin") continue ;;
            esac
            REASON="unexpected entry $entry; inspect and back up your data before repairing this prefix"
            REPAIRABLE=0
            return 0
        done
    done
    if [ -L "$iprefix/bin" ] || ! [ -d "$iprefix/bin" ]; then
        REASON="bin is not a real directory; bin/mind is missing or is not a regular file"; return 0
    fi
    if [ -L "$ibin" ] || ! [ -f "$ibin" ]; then REASON="bin/mind is missing or is not a regular file"; return 0; fi
    if ! [ -x "$ibin" ]; then REASON="bin/mind is not executable"; return 0; fi
    irec=$iprefix/install-record
    if [ -L "$irec" ]; then REASON="install-record is a symbolic link"; return 0; fi
    has_record=0
    if [ -e "$irec" ]; then
        if ! [ -f "$irec" ]; then REASON="install-record is not a regular file"; return 0; fi
        # Read as data, never source a receipt; reject duplicates, missing and unknown fields.
        if ! awk '
            NR == 1 {if ($0 != "mindspace-install-record 1") bad=1; next}
            {i=index($0, "="); k=substr($0, 1, i-1); v=substr($0, i+1)
             if (!i || !length(v) || index(v, "=") || seen[k]++) bad=1
             if (k == "installed_at" && v !~ /^[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9]Z$/) bad=1
             if (k !~ /^(version|archive_sha256|binary_sha256|source_commit|publisher_fingerprint|installed_at)$/) bad=1}
            END {if (NR != 7) bad=1; exit bad ? 1 : 0}' "$irec"; then
            REASON="install-record has an unknown format"; return 0
        fi
        rv=$(awk -F= '$1 == "version" {print $2}' "$irec")
        ra=$(awk -F= '$1 == "archive_sha256" {print $2}' "$irec")
        rb=$(awk -F= '$1 == "binary_sha256" {print $2}' "$irec")
        rf=$(awk -F= '$1 == "publisher_fingerprint" {print $2}' "$irec")
        rc=$(awk -F= '$1 == "source_commit" {print $2}' "$irec")
        if [ "$rv" != "$expected" ] || [ "$rf" != "$FPR" ] ||
            { [ "$expected" = "$VERSION" ] && { [ "$ra" != "$ARCHIVE_SHA" ] || [ "$rc" != "$MANIFEST_COMMIT" ]; }; }; then
            REASON="install-record describes a different version or build"; return 0
        fi
        if ! printf '%s\n' "$ra" | grep -Eq '^[0-9a-f]{64}$' ||
            ! printf '%s\n' "$rc" | grep -Eq '^[0-9a-f]{40}$'; then
            REASON="install-record has an unknown format"; return 0
        fi
        if [ "$rb" != "$(sha256sum "$ibin" | awk '{print $1}')" ]; then
            REASON="bin/mind no longer matches its install record (modified or damaged)"; return 0
        fi
        has_record=1
    fi
    igot=$(timeout -k 1 10 "$ibin" --version 2>/dev/null) || igot=
    if [ "$igot" != "mind $expected" ]; then
        REASON="bin/mind does not run, or reports '${igot:-nothing}' instead of 'mind $expected'"
        return 0
    fi
    STATE=legacy
    [ "$has_record" = 0 ] || STATE=valid
}

check_existing() {
    check_install_parents
    inspect_existing "$PREFIX" "$VERSION"
    if [ "$REINSTALL" != 1 ]; then
        case "$STATE" in
            valid | legacy)
                say "Already up to date: mind $VERSION runs at $PREFIX/bin/mind. Nothing was changed."
                if [ "$STATE" = legacy ]; then
                    say "It has no install record: legacy validation checks its layout and reported version only."
                    say "Its build authenticity cannot be re-established offline. For a freshly verified build, run: sh install.sh --reinstall"
                else
                    say "It matches its install record (a local consistency check, not independent authentication)."
                fi
                exit 0
                ;;
            invalid)
                say "$PREFIX is not a valid installation: $REASON." >&2
                say "Nothing was replaced. Inspect and back up this prefix first." >&2
                say "For a prefix containing only installer files, run: sh install.sh --reinstall" >&2
                say "The previous installation is kept under $INSTALL_ROOT/previous/, not deleted." >&2
                exit 1
                ;;
        esac
    fi
    NEWER=
    OLDER=
    # Validate sibling installations too; names alone cannot authorize a version transition.
    for d in "$INSTALL_ROOT"/*; do
        [ -e "$d" ] || [ -L "$d" ] || continue
        n=${d##*/}
        is_version "$n" || continue
        [ "$n" = "$VERSION" ] && continue
        inspect_existing "$d" "$n"
        [ "$STATE" != invalid ] || die "$d is not a valid installation: $REASON. Inspect and back up that prefix before retrying; nothing was replaced."
        if [ "$(ver_cmp "$n" "$VERSION")" = 1 ]; then NEWER="$NEWER $n"; else OLDER="$OLDER $n"; fi
    done
    inspect_existing "$PREFIX" "$VERSION"
    [ "$REPAIRABLE" = 1 ] || die "$PREFIX is not a valid installation: $REASON. Nothing was replaced."
    [ -z "$NEWER" ] || say "Newer versions installed:$NEWER"
    [ -z "$OLDER" ] || say "Older versions installed:$OLDER"
    if [ -n "$NEWER" ] && [ "$ALLOW_OLDER" != 1 ]; then
        die "a newer version is already installed ($NEWER). Nothing was replaced. Obtain the current installer, or use --allow-older to install $VERSION beside it; PATH and MCP registrations remain unchanged."
    fi
    if [ -n "$OLDER" ] && [ "$UPGRADE" != 1 ] && [ "$STATE" = none ]; then
        die "an older installation exists ($OLDER). Run sh install.sh --upgrade to install $VERSION beside it. Previous binaries, PATH and MCP registrations are preserved."
    fi
    if [ "$REINSTALL" = 1 ] && [ "$STATE" != none ]; then
        say "--reinstall: after staging and verification the existing installation is moved to $INSTALL_ROOT/previous/, not deleted."
    fi
}

if [ "$VERIFY_ONLY" != 1 ]; then check_existing; fi

# A relative TMPDIR would break the build; resolve it. Cargo reads a .cargo/config.toml from every
# parent directory of the build, and anybody who can write one of them (for example /tmp/.cargo on a
# shared machine) could make the build run their code. Refuse to build below such a directory.
TMP_BASE=${TMPDIR:-/tmp}
if ! [ -d "$TMP_BASE" ] || ! [ -w "$TMP_BASE" ]; then die "temporary directory $TMP_BASE is not writable (set TMPDIR)."; fi
TMP_BASE=$(cd "$TMP_BASE" && pwd -P) || die "cannot resolve the temporary directory $TMP_BASE."
if [ "$VERIFY_ONLY" != 1 ]; then
    d=$TMP_BASE
    while :; do
        for f in "$d/.cargo/config.toml" "$d/.cargo/config"; do
            if [ -e "$f" ] || [ -L "$f" ]; then
                die "Cargo would read $f while building here. Set TMPDIR to a private directory that has no .cargo/config.toml in it or above it, for example TMPDIR=\"\$HOME/tmp-install\" (create it first, mode 700)."
            fi
        done
        [ "$d" = / ] && break
        d=${d%/*}
        [ -n "$d" ] || d=/
    done
fi

MISSING=
for c in tar gzip gpg sed cmp cp mktemp install df wc; do need_cmd "$c"; done
[ -n "$ASSETS_DIR" ] || need_cmd curl
if [ "$VERIFY_ONLY" != 1 ]; then
    for c in setsid env tail sleep flock; do need_cmd "$c"; done
    if ! command -v cc >/dev/null 2>&1 && ! command -v gcc >/dev/null 2>&1; then
        MISSING="$MISSING cc(C-compiler)"
    fi
    if command -v rustup >/dev/null 2>&1; then
        :
    else
        need_cmd cargo
        need_cmd rustc
    fi
fi
if [ -n "$MISSING" ]; then
    die "missing required tools:$MISSING. Install them with your system's package manager, then run this again."
fi
gpg --dump-options 2>/dev/null | grep -q -- '--assert-signer' ||
    die "your GnuPG is too old: it must support --assert-signer. Update GnuPG; do not skip the check."

# Rust 1.98.1 is what the archive pins. Never use "stable" or the toolchain of the main branch.
CARGO_RUN=
if [ "$VERIFY_ONLY" != 1 ]; then
    if command -v rustup >/dev/null 2>&1; then
        rustup toolchain list 2>/dev/null | grep -q "^$RUST_VERSION-" ||
            die "Rust $RUST_VERSION is not installed. Run: rustup toolchain install $RUST_VERSION --profile minimal"
        RUSTUP_AUTO_INSTALL=0 rustup run "$RUST_VERSION" rustc --version 2>/dev/null | grep -q "^rustc $RUST_VERSION " ||
            die "rustup could not run Rust $RUST_VERSION."
        CARGO_RUN=rustup
    else
        if rustc --version 2>/dev/null | grep -q "^rustc $RUST_VERSION " &&
            cargo --version 2>/dev/null | grep -q "^cargo $RUST_VERSION "; then
            CARGO_RUN=plain
        else
            die "this Alpha builds with Rust $RUST_VERSION exactly; found: $(rustc --version 2>/dev/null || echo none). Install rustup and run: rustup toolchain install $RUST_VERSION --profile minimal"
        fi
    fi
fi

if [ "$VERIFY_ONLY" != 1 ]; then
    AVAIL=$(df -Pk "$TMP_BASE" 2>/dev/null | awk 'NR==2 {print $4}')
    case "$AVAIL" in
        '' | *[!0-9]*) die "cannot determine the free space in $TMP_BASE (set TMPDIR to a directory on a disk with about 2 GiB free)." ;;
    esac
    [ "$AVAIL" -ge "$NEED_KIB" ] || die "the build needs about 2 GiB of free space in $TMP_BASE; found $((AVAIL / 1024)) MiB. Set TMPDIR to a larger directory."
fi
say "Linux x86_64: ok. Tools: ok. Install prefix: $PREFIX"

# ---------------------------------------------------------------- 2. publisher trust decision
step "Publisher key: your trust decision"

cat <<EOF
Releases are signed by Christian Nachtigall <christian@nachtigall.dev>.
Primary key fingerprint (40 hex characters):

    $FPR_SPACED

This installer carries that fingerprint and also fetches the key from the publisher's own domain
(OpenPGP Web Key Directory, openpgpkey.nachtigall.dev). Both come from sources the project controls, so
neither proves who the publisher is. The release page and the files in it can not authenticate
themselves either.

YOU decide whom to trust: confirm this fingerprint through a channel you trust that is not this
download (for example from the maintainer directly or from a record you already hold). If you cannot
confirm it, stop here; see docs/install.md for the manual procedure.
EOF

if [ -z "$TRUST_FPR" ]; then
    if [ -t 0 ]; then
        say ""
        say "Type the fingerprint you have confirmed (spaces allowed), or press Enter to stop:"
        printf '> '
        IFS= read -r TRUST_FPR || TRUST_FPR=
        [ -n "$TRUST_FPR" ] || { say "No fingerprint entered. Nothing was changed."; exit 2; }
    else
        say "" >&2
        say "STOP: not running in a terminal, so there is nobody to ask. Confirm the fingerprint" >&2
        say "independently, then run: sh install.sh --trust-fingerprint <the 40 hex characters>" >&2
        exit 2
    fi
fi
TRUST_FPR=$(printf '%s' "$TRUST_FPR" | tr -d ' \t:' | tr 'abcdef' 'ABCDEF')
case "$TRUST_FPR" in
    *[!0-9A-F]*) die "the fingerprint you gave contains characters that are not hex digits." ;;
esac
[ "${#TRUST_FPR}" -eq 40 ] || die "a fingerprint has 40 hex characters; you gave ${#TRUST_FPR}."
[ "$TRUST_FPR" = "$FPR" ] || die "the fingerprint you gave does not match the one this installer pins ($FPR). Nothing was installed. If the publisher rotated keys, obtain a current install.sh from a source you trust."
say "Fingerprint accepted: you confirmed $FPR."

# ---------------------------------------------------------------- 3. assets
WORK=$(mktemp -d "$TMP_BASE/mindspace-install.XXXXXX") || die "cannot create a temporary directory."
chmod 700 "$WORK"
mkdir "$WORK/assets" "$WORK/gnupg" "$WORK/src"
chmod 700 "$WORK/gnupg"

step "Getting the seven release assets"

fetch() {
    curl --fail --silent --show-error --location --proto '=https' --proto-redir '=https' --tlsv1.2 \
        --connect-timeout 15 --max-time 300 --max-filesize 5000000 --output "$2" "$1"
}
sha_of() { sha256sum "$1" | awk '{print $1}'; }

while read -r want_sum want_size name; do
    dest=$WORK/assets/$name
    if [ -n "$ASSETS_DIR" ]; then
        if ! [ -f "$ASSETS_DIR/$name" ] || [ -L "$ASSETS_DIR/$name" ]; then
            die "$ASSETS_DIR/$name is missing (or is a link). All seven assets are required."
        fi
        cp -- "$ASSETS_DIR/$name" "$dest"
    else
        say "  downloading $name"
        fetch "$BASE_URL/$name" "$dest" || die "could not download $BASE_URL/$name"
    fi
    size=$(wc -c <"$dest" | tr -d ' ')
    [ "$size" = "$want_size" ] || die "$name has $size bytes, expected $want_size. Do not use it."
    got=$(sha_of "$dest")
    [ "$got" = "$want_sum" ] || die "$name does not match its pinned SHA-256 (got $got). Do not use it."
done <<EOF
$ASSET_TABLE
EOF
say "All seven assets match their pinned sizes and SHA-256 values."

# ---------------------------------------------------------------- 4. signatures
step "Verifying the signatures"

gpgx() { gpg --no-options --homedir "$WORK/gnupg" --batch --no-tty --no-autostart --no-auto-key-retrieve "$@"; }

if [ -n "$KEY_FILE" ]; then
    [ -f "$KEY_FILE" ] || die "key file $KEY_FILE not found."
    cp -- "$KEY_FILE" "$WORK/publisher-key"
    say "  using the key file you supplied"
else
    say "  fetching the publisher key from openpgpkey.nachtigall.dev"
    fetch "$WKD_URL" "$WORK/publisher-key" || die "could not fetch the publisher key from the publisher's domain. Retry, or pass --key-file with a key you obtained independently."
fi
gpgx --import "$WORK/publisher-key" >/dev/null 2>&1 || die "the publisher key could not be imported."
PRIMARY=$(gpgx --with-colons --fingerprint "$FPR" 2>/dev/null | awk -F: '$1 == "fpr" {print $10; exit}')
[ "$PRIMARY" = "$FPR" ] || die "the key does not have the pinned fingerprint. It is not the publisher's key."
# The key bundled in the release is only checked for consistency; it authenticates nobody.
BUNDLED=$(gpg --no-options --homedir "$WORK/gnupg" --batch --no-tty --no-autostart --show-keys --with-colons "$WORK/assets/mindspace-release-signing-key.asc" 2>/dev/null |
    awk -F: '$1 == "fpr" {print $10; exit}')
[ "$BUNDLED" = "$FPR" ] || die "the key file inside the release has a different fingerprint ($BUNDLED). Do not use this release."

for f in "$ARCHIVE" "$MANIFEST"; do
    status=$WORK/$f.status
    gpgx --assert-signer "$FPR" --status-fd 1 --verify "$WORK/assets/$f.asc" "$WORK/assets/$f" >"$status" 2>/dev/null ||
        die "signature check failed for $f. Do not use it."
    if grep -Eq '^\[GNUPG:\] (BADSIG|ERRSIG|EXPSIG|EXPKEYSIG|REVKEYSIG|KEYEXPIRED|SIGEXPIRED|KEYREVOKED|NO_PUBKEY|FAILURE|ERROR)( |$)' "$status"; then
        die "the signature or key for $f is invalid, expired, revoked or unavailable."
    fi
    grep -q '^\[GNUPG:\] GOODSIG ' "$status" || die "no good signature found for $f."
    awk -v fpr="$FPR" '$1 == "[GNUPG:]" && $2 == "VALIDSIG" && $NF == fpr {ok = 1} END {exit ok ? 0 : 1}' "$status" ||
        die "$f is not signed by the primary key $FPR."
    say "  $f: good signature by $FPR"
done

# The two pinned checksums must also be what the (pinned, unsigned) SHA256SUMS file says.
printf '%s\n' \
    "$ARCHIVE_SHA  $ARCHIVE" \
    "903d8fd226593b74083bc8c0a11b7ffafc520eae06b5f054d0565ad1f5dae391  $MANIFEST" >"$WORK/expected-SHA256SUMS"
cmp -s "$WORK/expected-SHA256SUMS" "$WORK/assets/SHA256SUMS" || die "SHA256SUMS differs from the pinned values."
(cd "$WORK/assets" && sha256sum --check --strict SHA256SUMS >/dev/null) || die "sha256sum --check failed."
# The signed manifest must describe exactly this archive and the expected source commit.
grep -qF "\"sha256\": \"$ARCHIVE_SHA\"" "$WORK/assets/$MANIFEST" || die "the signed manifest does not describe this archive."
grep -qF "\"commit\": \"$MANIFEST_COMMIT\"" "$WORK/assets/$MANIFEST" || die "the signed manifest names an unexpected source commit."
say "Authenticated: both signatures are good, and archive, manifest and checksums agree."

if [ "$VERIFY_ONLY" = 1 ]; then
    say ""
    say "--verify-only: stopping here. Nothing was built or installed."
    exit 0
fi

# ---------------------------------------------------------------- 5. unpack safely
step "Unpacking the verified archive"

tar -tzf "$WORK/assets/$ARCHIVE" >"$WORK/names" 2>/dev/null || die "the archive cannot be listed."
tar -tzvf "$WORK/assets/$ARCHIVE" 2>/dev/null | cut -c1 >"$WORK/types" || die "the archive cannot be listed."
[ "$(wc -l <"$WORK/names")" = "$(wc -l <"$WORK/types")" ] || die "the archive listing is inconsistent."
awk -v top="$TOPDIR" '
    $0 == top || $0 == top "/" {next}
    index($0, top "/") != 1 {bad = 1}
    $0 ~ /(^|\/)\.\.(\/|$)/ {bad = 1}
    END {exit bad ? 1 : 0}' "$WORK/names" || die "the archive contains paths outside $TOPDIR/."
if grep -qv '^[-d]$' "$WORK/types"; then
    die "the archive contains links or special files, which this release does not have."
fi
tar -xzf "$WORK/assets/$ARCHIVE" -C "$WORK/src" --no-same-owner --no-same-permissions || die "extraction failed."
SRC=$WORK/src/$TOPDIR
if ! [ -f "$SRC/Cargo.toml" ] || ! [ -f "$SRC/Cargo.lock" ]; then die "the archive does not look like the Mindspace source."; fi
grep -q "^version = \"$VERSION\"" "$SRC/Cargo.toml" || die "the archive's version is not $VERSION."
grep -q "^channel = \"$RUST_VERSION\"" "$SRC/rust-toolchain.toml" || die "the archive pins a different Rust toolchain than $RUST_VERSION."
say "Unpacked to a private temporary directory."

# ---------------------------------------------------------------- 6. build
step "Building with Rust $RUST_VERSION (a few minutes; Cargo downloads the locked dependencies into the temporary directory)"

(
    cd "$SRC"
    unset RUSTFLAGS CARGO_ENCODED_RUSTFLAGS CARGO_BUILD_RUSTFLAGS RUSTDOCFLAGS RUSTC_WRAPPER RUSTC_WORKSPACE_WRAPPER RUSTC CARGO_BUILD_TARGET CARGO_PROFILE_RELEASE_LTO
    # An empty value overrides a rustc wrapper (for example sccache) set in a Cargo config file.
    export CARGO_BUILD_RUSTC_WRAPPER='' CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER=''
    # A private Cargo home: your ~/.cargo (its configuration, registry and cache) is neither read
    # nor written; the locked dependencies are downloaded into the temporary directory.
    unset CARGO_TARGET_DIR CARGO_BUILD_BUILD_DIR
    mkdir -p "$WORK/cargo-home"
    export CARGO_HOME="$WORK/cargo-home"
    if [ "$CARGO_RUN" = rustup ]; then
        set -- env RUSTUP_AUTO_INSTALL=0 RUSTUP_TOOLCHAIN="$RUST_VERSION" cargo build --locked --release --no-default-features --bin mind --target-dir "$WORK/target"
    else
        set -- cargo build --locked --release --no-default-features --bin mind --target-dir "$WORK/target"
    fi
    # Its own session and process group (its id is written to build.pgid), so an interruption can stop
    # the whole build tree: cargo, rustc, build scripts and the C compiler.
    # shellcheck disable=SC2016  # the inner shell, not this one, expands $$ and "$@"
    exec setsid -w sh -c 'echo "$$" >"$0"; exec "$@"' "$WORK/build.pgid" "$@"
) >"$WORK/build.log" 2>&1 &
BUILD_PID=$!
if wait "$BUILD_PID"; then
    BUILD_PID=
else
    BUILD_PID=
    say "The last lines of the build log:" >&2
    tail -n 25 "$WORK/build.log" >&2 || true
    die "the build failed. Nothing was installed."
fi
BIN=$WORK/target/release/mind
[ -x "$BIN" ] || die "the build did not produce a mind binary."
GOT_VERSION=$("$BIN" --version 2>/dev/null) || die "the built binary does not run."
[ "$GOT_VERSION" = "mind $VERSION" ] || die "the built binary reports '$GOT_VERSION', expected 'mind $VERSION'."
say "Built: $GOT_VERSION"

# ---------------------------------------------------------------- 7. install
step "Installing"

# Lock the real directory inode, avoiding a lock file on the read-only repeat path.
check_install_parents
(umask 022 && mkdir -p "$INSTALL_ROOT") || die "cannot create $INSTALL_ROOT."
exec 9<"$INSTALL_ROOT"
flock -n 9 || die "another installer is publishing into $INSTALL_ROOT. Let it finish, then retry."
# Repeat all checks under the publication lock: an install may have appeared during the build.
check_existing
STAGED=$(mktemp -d "$INSTALL_ROOT/.install.XXXXXX") || die "cannot stage the installation."
mkdir -m 700 "$STAGED/bin"
install -m 0755 "$BIN" "$STAGED/bin/mind" || die "cannot stage the binary; the previous installation is untouched."
[ "$("$STAGED/bin/mind" --version 2>/dev/null)" = "mind $VERSION" ] || die "the staged binary does not report 'mind $VERSION'."
INSTALLED_SHA=$(sha_of "$STAGED/bin/mind")
[ "$INSTALLED_SHA" = "$(sha_of "$BIN")" ] || die "the staged binary differs from the verified build."
{
    printf 'mindspace-install-record 1\n'
    printf 'version=%s\n' "$VERSION"
    printf 'binary_sha256=%s\n' "$INSTALLED_SHA"
    printf 'archive_sha256=%s\n' "$ARCHIVE_SHA"
    printf 'source_commit=%s\n' "$MANIFEST_COMMIT"
    printf 'publisher_fingerprint=%s\n' "$FPR"
    printf 'installed_at=%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
} >"$STAGED/install-record" || die "cannot write the install record."
chmod 600 "$STAGED/install-record"
# Staging can take time: refuse to move data added to the existing prefix meanwhile.
if [ "$REINSTALL" = 1 ]; then check_existing; fi
if [ "$REINSTALL" = 1 ] && [ "$STATE" != none ]; then
    BACKUP_DIR=$INSTALL_ROOT/previous
    if [ -L "$BACKUP_DIR" ] || { [ -e "$BACKUP_DIR" ] && [ ! -d "$BACKUP_DIR" ]; }; then
        die "$BACKUP_DIR is not a real directory. Inspect it before retrying; nothing was replaced."
    fi
    if [ ! -d "$BACKUP_DIR" ]; then mkdir -m 700 "$BACKUP_DIR"; fi
    BACKUP_HOLDER=$(mktemp -d "$BACKUP_DIR/$VERSION.XXXXXX") || die "cannot reserve a backup directory."
    BACKUP=$BACKUP_HOLDER/install
    # Set intent first so cleanup handles a signal delivered immediately after the rename.
    MOVED_OLD=1
    mv -T -- "$PREFIX" "$BACKUP" || die "cannot move the existing installation aside."
    say "The previous installation is kept at $BACKUP."
fi
if [ -e "$PREFIX" ] || [ -L "$PREFIX" ]; then die "$PREFIX appeared meanwhile; it was left untouched."; fi
# -T prevents nesting a replacement inside an unexpectedly created destination directory.
if ! mv -T --no-clobber "$STAGED" "$PREFIX"; then
    # Coreutils versions differ on the exit status of a skipped no-clobber rename.
    if [ -d "$STAGED" ] && { [ -e "$PREFIX" ] || [ -L "$PREFIX" ]; }; then
        die "$PREFIX appeared meanwhile; it was left untouched."
    fi
    die "cannot publish the staged installation."
fi
[ ! -d "$STAGED" ] || die "$PREFIX appeared meanwhile; it was left untouched."
INSTALL_OK=1
STAGED=
flock -u 9

cat <<EOF

Installed and verified.

  binary   $PREFIX/bin/mind
  version  mind $VERSION
  sha256   $INSTALLED_SHA

Nothing else was changed: no PATH entry, no shell startup file, no agent configuration, no database.
${BACKUP:+The previous installation is kept at $BACKUP.
}
Next steps
  1. Try it without changing your shell (works in sh, bash, zsh and fish):
       $PREFIX/bin/mind --version
       $PREFIX/bin/mind --db "\$HOME/mindspace-demo/mind.db" init
  2. Optional: put it on your PATH yourself.
       bash/zsh:  export PATH="$PREFIX/bin:\$PATH"
       fish:      fish_add_path $PREFIX/bin
  3. Follow the quickstart: https://github.com/VikingOwl91/mindspace#quickstart
  4. Connect an agent (shows a preview first, changes nothing until you add --apply):
       $PREFIX/bin/mind setup codex
       $PREFIX/bin/mind setup claude-code

To remove it later: rm -- "$PREFIX/bin/mind" "$PREFIX/install-record" && rmdir "$PREFIX/bin" "$PREFIX"
Your databases live elsewhere and are not removed.
EOF
