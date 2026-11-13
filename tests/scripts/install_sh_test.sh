#!/bin/sh
# Tests for install.sh. Run from anywhere:  sh tests/scripts/install_sh_test.sh [--with-build]
#
# Default run: no build. It downloads the seven release assets and the publisher key once (public
# GitHub and the publisher's domain, no credentials), then runs the real installer in isolated
# temporary HOME directories and checks that it refuses what it must refuse and writes nothing it
# should not.
# --with-build also performs real installs (needs Rust 1.98.1, a C compiler, free disk and network
# for Cargo): fresh install, rerun, existing database, interruption, the CLI quickstart and the MCP
# setup preview.
set -u

ROOT=$(cd "$(dirname "$0")/../.." && pwd)
INSTALL=$ROOT/install.sh
FPR=0E6699CB48F3941AE28CA88D509F78007F62D5EA
BASE=https://github.com/VikingOwl91/mindspace/releases/download/v0.1.0-alpha.1
WKD='https://openpgpkey.nachtigall.dev/.well-known/openpgpkey/nachtigall.dev/hu/rckmfa7rw857sf15hkixzhxx4681turz?l=christian'
ARCHIVE=mindspace-0.1.0-alpha.1-rc.5-source.tar.gz
MANIFEST=source-archive-manifest.json
WITH_BUILD=0
[ "${1:-}" = --with-build ] && WITH_BUILD=1

T=$(mktemp -d /tmp/mindspace-installtest.XXXXXX) || exit 1
FAILS=0
N=0
cleanup() {
    gpgconf --homedir "$T/gk" --kill all >/dev/null 2>&1 || true
    rm -rf -- "$T"
}
trap cleanup EXIT
trap 'exit 130' INT TERM

pass() { N=$((N + 1)); printf 'ok   %s\n' "$1"; }
fail() { N=$((N + 1)); FAILS=$((FAILS + 1)); printf 'FAIL %s\n' "$1"; [ -z "${2:-}" ] || printf '%s\n' "$2" | sed 's/^/       /'; }

fetch() { curl -q --fail --silent --show-error --location --proto '=https' --tlsv1.2 --connect-timeout 15 --max-time 120 --output "$2" "$1"; }

# run_inst NAME [ARGS...]: runs the installer in a clean environment with its own HOME; sets OUT, RC.
new_home() { H=$T/home-$1; mkdir -p "$H" "$T/tmp-$1"; }
run_inst() {
    OUT=$(env -i HOME="$H" PATH="${TEST_PATH:-$PATH}" TMPDIR="$T/tmp-$HN" RUSTUP_HOME="${RUSTUP_HOME:-$HOME/.rustup}" \
        sh "${INST:-$INSTALL}" --version 0.1.0-alpha.1 --assets-dir "$T/assets" "$@" </dev/null 2>&1)
    RC=$?
}
with_path() { TEST_PATH=$1; shift; "$@"; TEST_PATH=; }
expect() { # NAME WANT_RC PATTERN
    if [ "$RC" = "$2" ] && printf '%s' "$OUT" | grep -q -- "$3"; then pass "$1"; else fail "$1 (rc=$RC, wanted $2 and /$3/)" "$(printf '%s' "$OUT" | tail -n 6)"; fi
}
tree_of() { (cd "$1" && find . -printf '%p %y %m %s %T@ %l\n' | sort; find . -type f -exec sha256sum {} + | sort); }
no_leftovers() { # NAME: the installer's temp dir is gone
    left=0
    for d in "$T/tmp-$HN"/mindspace-install.*; do [ -e "$d" ] && left=1; done
    if [ "$left" = 1 ]; then fail "$1: temporary directory left behind"; else pass "$1: temporary directory cleaned up"; fi
    if pgrep -f "$T/tmp-$HN" >/dev/null 2>&1; then fail "$1: a process of the installer is still running"; else pass "$1: no installer process left (build tools, gpg-agent)"; fi
}

printf 'Setup: downloading the release assets and the publisher key\n'
mkdir -p "$T/assets"
for f in "$ARCHIVE" "$ARCHIVE.asc" "$MANIFEST" "$MANIFEST.asc" SHA256SUMS VERIFY.txt mindspace-release-signing-key.asc; do
    fetch "$BASE/$f" "$T/assets/$f" || { echo "setup: cannot download $f"; exit 1; }
done
fetch "$WKD" "$T/key.asc" || { echo "setup: cannot fetch the publisher key"; exit 1; }

# --------------------------------------------------------------------------- static
sh -n "$INSTALL" && pass "install.sh parses with sh" || fail "install.sh does not parse"
if command -v shellcheck >/dev/null 2>&1; then
    if shellcheck -s sh "$INSTALL"; then pass "shellcheck (POSIX sh) is clean"; else fail "shellcheck reports issues"; fi
fi

# --------------------------------------------------------------------------- environment refusals
HN=os; new_home os
mkdir -p "$T/shim-os"
cat >"$T/shim-os/uname" <<'EOF'
#!/bin/sh
case "$1" in -s) echo Darwin ;; -m) echo arm64 ;; *) echo Darwin ;; esac
EOF
chmod +x "$T/shim-os/uname"
with_path "$T/shim-os:$PATH" run_inst --verify-only --trust-fingerprint "$FPR"
expect "unsupported OS is refused" 1 "Linux only"
mkdir -p "$T/shim-arch"
printf '#!/bin/sh\ncase "$1" in -m) echo aarch64 ;; *) echo Linux ;; esac\n' >"$T/shim-arch/uname"
chmod +x "$T/shim-arch/uname"
with_path "$T/shim-arch:$PATH" run_inst --verify-only --trust-fingerprint "$FPR"
expect "unsupported architecture is refused" 1 "x86_64 only"

# A PATH without gpg / without a Rust toolchain.
mkshim() { # DIR omitted-tool...
    d=$1; shift; mkdir -p "$d"
    for tool in sh date timeout flock python3 uname setsid pgrep sha256sum tar gzip gpg awk grep sed tr cmp mktemp install df wc curl cc gcc cut mv cp mkdir rmdir rm chmod cat head tail env dirname sleep cargo rustc rustup; do
        skip=0
        for o in "$@"; do [ "$o" = "$tool" ] && skip=1; done
        [ "$skip" = 1 ] && continue
        p=$(command -v "$tool" 2>/dev/null) && [ -x "$p" ] || continue
        p=$(cd "$(dirname "$p")" && pwd)/${p##*/}
        ln -sf "$p" "$d/$tool"
    done
}
HN=dep; new_home dep
mkshim "$T/shim-nogpg" gpg
with_path "$T/shim-nogpg" run_inst --verify-only --trust-fingerprint "$FPR"
expect "missing gpg is reported" 1 "missing required tools:.*gpg"
mkshim "$T/shim-norust" cargo rustc rustup
with_path "$T/shim-norust" run_inst --trust-fingerprint "$FPR"
expect "missing Rust is reported before any download" 1 "missing required tools:.*cargo"

# --------------------------------------------------------------------------- existing installation
# Re-running the installer must be safe: an intact install is reported and left alone, a damaged or
# unexpected one is never trusted or replaced silently, and other versions are never downgraded.
CUR=0.1.0-alpha.1
PIN_ARCHIVE=b587a04edff66963162267fff805678b7c1ba8aefbaec795e518702e0e4149c0
expect_out() { # NAME PATTERN: the output contains PATTERN (exit code not checked)
    if printf '%s' "$OUT" | grep -q -- "$2"; then pass "$1"; else fail "$1 (no /$2/ in the output)" "$(printf '%s' "$OUT" | tail -n 6)"; fi
}
expect_not_out() { # NAME PATTERN
    if printf '%s' "$OUT" | grep -q -- "$2"; then fail "$1 (found /$2/)" "$(printf '%s' "$OUT" | tail -n 6)"; else pass "$1"; fi
}
case_home() { HN=$1; new_home "$1"; }
mk_prefix() { # HOMEDIR VERSION REPORTED-VERSION RECORD(ok|none|stale|other): a fake installed binary
    pd=$1/.local/lib/mindspace/$2
    mkdir -p "$pd/bin"
    printf '#!/bin/sh\necho "mind %s"\n' "$3" >"$pd/bin/mind"
    chmod 755 "$pd/bin/mind"
    case "$4" in
        none) ;;
        *)
            sum=$(sha256sum "$pd/bin/mind" | awk '{print $1}')
            if [ "$4" = stale ]; then sum=0000000000000000000000000000000000000000000000000000000000000000; fi
            ver=$2
            if [ "$4" = other ]; then ver=0.0.1; fi
            printf 'mindspace-install-record 1\nversion=%s\nbinary_sha256=%s\narchive_sha256=%s\nsource_commit=a027d94680392648b6277dc2fd9bc20906114b91\npublisher_fingerprint=%s\ninstalled_at=2026-11-13T12:00:00Z\n' "$ver" "$sum" "$PIN_ARCHIVE" "$FPR" >"$pd/install-record"
            ;;
    esac
}
untouched() { # NAME: HOME looks exactly as it did in $before
    if [ "$before" = "$(tree_of "$H")" ]; then pass "$1: nothing was changed"; else fail "$1: HOME was modified"; fi
}
inst_state() { # NAME RECORD REPORTED WANT_RC PATTERN
    case_home "$1"; mk_prefix "$H" "$CUR" "$3" "$2"
    before=$(tree_of "$H"); content=$(cat "$H/.local/lib/mindspace/$CUR/bin/mind")
    run_inst --trust-fingerprint "$FPR" --assets-dir "$T/does-not-exist"
    expect "$1" "$4" "$5"
    [ "$content" = "$(cat "$H/.local/lib/mindspace/$CUR/bin/mind")" ] || fail "$1: the binary changed"
    untouched "$1"
    no_leftovers "$1"
}
inst_state "intact install is reported as already up to date" ok "$CUR" 0 "Already up to date"
inst_state "install without a record is reported, not trusted" none "$CUR" 0 "no install record"
inst_state "binary that no longer matches its record is invalid" stale "$CUR" 1 "no longer matches"
inst_state "record of another version is invalid" other "$CUR" 1 "different version or build"
inst_state "binary reporting another version is invalid" none 9.9.9 1 "reports '"

case_home idem; mk_prefix "$H" "$CUR" "$CUR" ok
before=$(tree_of "$H")
run_inst --trust-fingerprint "$FPR"; expect "running twice: first run is already up to date" 0 "Already up to date"
run_inst --trust-fingerprint "$FPR"; expect "running twice: second run is already up to date" 0 "Already up to date"
untouched "repeated runs"
case_home notools; mk_prefix "$H" "$CUR" "$CUR" ok
with_path "$T/shim-norust" run_inst
expect "an intact install needs no Rust, no network and no fingerprint" 0 "Already up to date"

case_home inv_exec; mk_prefix "$H" "$CUR" "$CUR" none; chmod 644 "$H/.local/lib/mindspace/$CUR/bin/mind"
before=$(tree_of "$H"); run_inst --trust-fingerprint "$FPR"; expect "non-executable binary is invalid" 1 "not executable"; untouched "non-executable binary"
case_home inv_nobin; mkdir -p "$H/.local/lib/mindspace/$CUR"
before=$(tree_of "$H"); run_inst --trust-fingerprint "$FPR"; expect "an empty directory is not an installation" 1 "missing or is not a regular file"; untouched "empty directory"
case_home inv_link; mkdir -p "$H/real" "$H/.local/lib/mindspace"; ln -s "$H/real" "$H/.local/lib/mindspace/$CUR"
before=$(tree_of "$H"); run_inst --trust-fingerprint "$FPR"; expect "a symlinked prefix is invalid" 1 "symbolic link"; untouched "symlinked prefix"
case_home inv_file; mkdir -p "$H/.local/lib/mindspace"; printf x >"$H/.local/lib/mindspace/$CUR"
before=$(tree_of "$H"); run_inst --trust-fingerprint "$FPR"; expect "a file in place of the prefix is invalid" 1 "not a directory"; untouched "file in place of the prefix"
case_home inv_binlink; mk_prefix "$H" "$CUR" "$CUR" none; mv "$H/.local/lib/mindspace/$CUR/bin/mind" "$H/realbin"; ln -s "$H/realbin" "$H/.local/lib/mindspace/$CUR/bin/mind"
before=$(tree_of "$H"); run_inst --trust-fingerprint "$FPR"; expect "a symlinked binary is invalid" 1 "regular file"; untouched "symlinked binary"
case_home inv_hint; mk_prefix "$H" "$CUR" 9.9.9 none
before=$(tree_of "$H"); run_inst --trust-fingerprint "$FPR"
expect "an invalid install gets recovery guidance" 1 "sh install.sh --reinstall"
expect_out "the guidance says nothing is deleted" "not deleted"

# --reinstall is explicit, and the old install stays in place until a build has succeeded.
case_home re; mk_prefix "$H" "$CUR" 9.9.9 none
before=$(tree_of "$H")
run_inst --reinstall --trust-fingerprint "$FPR" --assets-dir "$T/does-not-exist"
expect_out "--reinstall says what it will do" "moved to"
untouched "--reinstall before any build"

# Versions installed beside this one: never downgrade silently, and understand prerelease ordering.
trans() { # NAME OTHER-VERSION older|newer
    case_home "$1"; mk_prefix "$H" "$2" "$2" ok
    before=$(tree_of "$H")
    run_inst --trust-fingerprint "$FPR" --assets-dir "$T/does-not-exist"
    if [ "$3" = newer ]; then
        expect "$2 is newer than $CUR: the installer refuses to go back" 1 "a newer version is already installed"
        run_inst --allow-older --trust-fingerprint "$FPR" --assets-dir "$T/does-not-exist"
        expect_out "$2: --allow-older is accepted" "Newer versions installed: $2"
        expect_not_out "$2: --allow-older is not refused" "a newer version is already installed"
    else
        expect_out "$2 is older than $CUR: noted, not an obstacle" "Older versions installed: $2"
        expect_not_out "$2: an older version does not block" "a newer version is already installed"
    fi
    untouched "$2 beside $CUR"
}
trans tv1 0.0.9 older
trans tv2 0.1.0-alpha.0 older
trans tv3 0.1.0-alpha older
trans tv4 0.1.0-alpha.2 newer
trans tv5 0.1.0-alpha.10 newer
trans tv6 0.1.0-beta.1 newer
trans tv7 0.1.0 newer
trans tv8 0.2.0 newer
trans tv9 1.0.0 newer
case_home tv_ignored
mkdir -p "$H/.local/lib/mindspace/previous/9.9.9-1" "$H/.local/lib/mindspace/0.9.9.bak/bin" "$H/.local/lib/mindspace/2.0.0"
printf x >"$H/.local/lib/mindspace/0.9.9.bak/bin/mind"
run_inst --trust-fingerprint "$FPR" --assets-dir "$T/does-not-exist"
expect_not_out "backups and odd names never authorize a transition" "a newer version is already installed"
expect "a bare version directory is invalid" 1 "not a valid installation"
case_home tv_current_newer; mk_prefix "$H" "$CUR" "$CUR" ok; mk_prefix "$H" 0.2.0 0.2.0 ok
run_inst --trust-fingerprint "$FPR"
expect "an intact install stays up to date even when a newer one exists" 0 "Already up to date"
expect_not_out "a no-op needs no version transition" "STOP:"

# --------------------------------------------------------------------------- trust decision
HN=trust; new_home trust
before=$(tree_of "$H")
run_inst --assets-dir "$T/assets" --verify-only
expect "no trust decision without a terminal is refused" 2 "not running in a terminal"
run_inst --assets-dir "$T/assets" --verify-only --trust-fingerprint "0E6699CB48F3941AE28CA88D509F78007F62D5EB"
expect "a different fingerprint is refused" 1 "does not match"
run_inst --assets-dir "$T/assets" --verify-only --trust-fingerprint "0E6699CB"
expect "a short fingerprint is refused" 1 "40 hex"
run_inst --assets-dir "$T/assets" --verify-only --trust-fingerprint "not-hex-not-hex-not-hex-not-hex-not-hex-xx"
expect "a non-hex fingerprint is refused" 1 "not hex"
[ "$before" = "$(tree_of "$H")" ] && pass "trust refusals write nothing to HOME" || fail "trust refusals wrote to HOME"
no_leftovers "trust refusals"

# --------------------------------------------------------------------------- asset integrity
HN=assets; new_home assets
before=$(tree_of "$H")
for f in "$ARCHIVE" "$ARCHIVE.asc" "$MANIFEST" "$MANIFEST.asc" SHA256SUMS VERIFY.txt mindspace-release-signing-key.asc; do
    rm -rf "$T/bad"; mkdir "$T/bad"; cp "$T/assets/"* "$T/bad/"
    printf 'x' >>"$T/bad/$f"
    run_inst --assets-dir "$T/bad" --verify-only --trust-fingerprint "$FPR" --key-file "$T/key.asc"
    expect "modified $f is rejected" 1 "bytes, expected"
done
rm -rf "$T/bad"; mkdir "$T/bad"; cp "$T/assets/"* "$T/bad/"
# same size, different content
orig=$(dd if="$T/bad/$ARCHIVE" bs=1 skip=100 count=1 2>/dev/null | od -An -tu1 | tr -d ' ')
new=$(( (orig + 1) % 256 ))
printf "\\$(printf '%03o' "$new")" | dd of="$T/bad/$ARCHIVE" bs=1 seek=100 conv=notrunc 2>/dev/null
run_inst --assets-dir "$T/bad" --verify-only --trust-fingerprint "$FPR" --key-file "$T/key.asc"
expect "a flipped bit in the archive is rejected" 1 "pinned SHA-256"
rm -rf "$T/bad"; mkdir "$T/bad"; cp "$T/assets/"* "$T/bad/"; rm "$T/bad/VERIFY.txt"
run_inst --assets-dir "$T/bad" --verify-only --trust-fingerprint "$FPR" --key-file "$T/key.asc"
expect "a missing asset is rejected" 1 "VERIFY.txt is missing"
[ "$before" = "$(tree_of "$H")" ] && pass "integrity refusals write nothing to HOME" || fail "integrity refusals wrote to HOME"
no_leftovers "integrity refusals"

# --------------------------------------------------------------------------- signature layer
# The pinned SHA-256 values are the first defence. To prove the signature check works on its own,
# these cases use a copy of the installer whose pins were updated to the tampered files.
HN=sig; new_home sig
mkdir -p "$T/gk"; chmod 700 "$T/gk"
gpg --homedir "$T/gk" --batch --quiet --pinentry-mode loopback --passphrase '' --quick-generate-key 'Test Key <test@example.invalid>' ed25519 sign never >/dev/null 2>&1
gpg --homedir "$T/gk" --batch --quiet --armor --export >"$T/wrongkey.pub"
repin() { # DIR NAME: rewrite the pin for NAME in a copy of the installer
    new=$(sha256sum "$1/$2" | awk '{print $1}'); size=$(wc -c <"$1/$2" | tr -d ' ')
    old=$(awk -v n="$2" '{k = $3; gsub(/\047/, "", k)} k == n {s = $1; sub(/^ASSET_TABLE=./, "", s); print s}' "$INSTALL" | head -n1)
    oldsize=$(awk -v n="$2" '{k = $3; gsub(/\047/, "", k)} k == n {print $2}' "$INSTALL" | head -n1)
    sed "s/^\\(ASSET_TABLE='\\)\\{0,1\\}$old $oldsize $2/\\1$new $size $2/" "${INST:-$INSTALL}" >"$T/inst-mod.sh"
    INST=$T/inst-mod.sh
}
INST=
rm -rf "$T/bad"; mkdir "$T/bad"; cp "$T/assets/"* "$T/bad/"
printf 'tampered' >>"$T/bad/$ARCHIVE"
repin "$T/bad" "$ARCHIVE"
run_inst --assets-dir "$T/bad" --verify-only --trust-fingerprint "$FPR" --key-file "$T/key.asc"
expect "a tampered archive with a matching pin fails the signature check" 1 "signature check failed"
INST=
rm -rf "$T/bad"; mkdir "$T/bad"; cp "$T/assets/"* "$T/bad/"
gpg --homedir "$T/gk" --batch --quiet --yes --armor --detach-sign -o "$T/bad/$ARCHIVE.asc" "$T/bad/$ARCHIVE"
repin "$T/bad" "$ARCHIVE.asc"
run_inst --assets-dir "$T/bad" --verify-only --trust-fingerprint "$FPR" --key-file "$T/key.asc"
expect "a valid signature by another key is rejected" 1 "signature check failed"
INST=
run_inst --assets-dir "$T/assets" --verify-only --trust-fingerprint "$FPR" --key-file "$T/wrongkey.pub"
expect "a substituted publisher key is rejected" 1 "pinned fingerprint"
rm -rf "$T/bad"; mkdir "$T/bad"; cp "$T/assets/"* "$T/bad/"
cp "$T/wrongkey.pub" "$T/bad/mindspace-release-signing-key.asc"
repin "$T/bad" "mindspace-release-signing-key.asc"
run_inst --assets-dir "$T/bad" --verify-only --trust-fingerprint "$FPR" --key-file "$T/key.asc"
expect "a different bundled key in the release is rejected" 1 "different fingerprint"
INST=

# --------------------------------------------------------------------------- authentication succeeds
HN=ok; new_home ok
before=$(tree_of "$H")
run_inst --assets-dir "$T/assets" --verify-only --trust-fingerprint "$FPR" --key-file "$T/key.asc"
expect "verify-only with local assets and the key file succeeds" 0 "Authenticated"
run_inst --verify-only --trust-fingerprint "$(printf '%s' "$FPR" | sed 's/\(....\)/\1 /g')" --assets-dir ""
expect "verify-only with real downloads and the key from the publisher domain succeeds (spaced fingerprint)" 0 "Authenticated"
[ "$before" = "$(tree_of "$H")" ] && pass "verify-only leaves HOME untouched" || fail "verify-only wrote to HOME"
no_leftovers "verify-only"
for d in .codex .claude .config .gnupg .cargo .bashrc .profile .zshrc; do
    [ -e "$H/$d" ] && fail "unexpected $d in HOME"
done

# --------------------------------------------------------------------------- build directory hygiene
# Cargo reads .cargo/config.toml from every parent of the build directory: refuse to build below one.
HN=cc; new_home cc
mkdir -p "$T/cc/tmp" "$T/cc/.cargo"
printf '[build]\n' >"$T/cc/.cargo/config.toml"
OUT=$(env -i HOME="$H" PATH="$PATH" TMPDIR="$T/cc/tmp" sh "$INSTALL" --version 0.1.0-alpha.1 --trust-fingerprint "$FPR" --assets-dir "$T/does-not-exist" </dev/null 2>&1); RC=$?
expect "a Cargo config above the build directory is refused before any work" 1 "Cargo would read"
OUT=$(env -i HOME="$H" PATH="$PATH" TMPDIR="$T/cc/tmp" sh "$INSTALL" --version 0.1.0-alpha.1 --verify-only --trust-fingerprint "$FPR" --assets-dir "$T/assets" --key-file "$T/key.asc" </dev/null 2>&1); RC=$?
expect "the same directory is fine for --verify-only (nothing is built)" 0 "Authenticated"
HN=rel; new_home rel
mkdir -p "$T/rel/reltmp"
OUT=$(cd "$T/rel" && env -i HOME="$H" PATH="$PATH" TMPDIR=reltmp sh "$INSTALL" --version 0.1.0-alpha.1 --verify-only --trust-fingerprint "$FPR" --assets-dir "$T/assets" --key-file "$T/key.asc" </dev/null 2>&1); RC=$?
expect "a relative TMPDIR is resolved" 0 "Authenticated"

# --------------------------------------------------------------------------- the typed fingerprint prompt
if command -v script >/dev/null 2>&1; then
    HN=pty; new_home pty
    tty_run() { # ANSWER: runs the installer on a pseudo terminal and types ANSWER
        OUT=$(printf '%s\n' "$1" | env -i HOME="$H" PATH="$PATH" TERM=dumb TMPDIR="$T/tmp-$HN" \
            script -qec "sh $INSTALL --version 0.1.0-alpha.1 --assets-dir $T/assets --verify-only --key-file $T/key.asc" /dev/null 2>&1)
        RC=$?
    }
    tty_run "$FPR"
    if printf '%s' "$OUT" | grep -q 'Type the fingerprint'; then
        expect "typing the confirmed fingerprint at the prompt is accepted" 0 "Fingerprint accepted"
        tty_run "0E6699CB48F3941AE28CA88D509F78007F62D5EB"
        expect "a wrong typed fingerprint is refused" 1 "does not match"
        tty_run ""
        expect "an empty answer stops and changes nothing" 2 "Nothing was changed"
    else
        printf 'skip script(1) cannot drive a terminal here\n'
    fi
fi

# --------------------------------------------------------------------------- the signature status layer
# A key that has already expired, used as "the publisher" in a modified copy of the installer, shows
# that an otherwise valid signature by an expired key is refused (the layer beyond --assert-signer).
HN=exp; new_home exp
mkdir -p "$T/gk2"; chmod 700 "$T/gk2"
if gpg --homedir "$T/gk2" --batch --quiet --pinentry-mode loopback --passphrase '' --faked-system-time 20200101T000000 \
    --quick-generate-key 'Expired Key <expired@example.invalid>' ed25519 sign 1d >/dev/null 2>&1; then
    FP2=$(gpg --homedir "$T/gk2" --batch --with-colons --fingerprint 2>/dev/null | awk -F: '$1 == "fpr" {print $10; exit}')
    gpg --homedir "$T/gk2" --batch --quiet --armor --export >"$T/expired.pub"
    rm -rf "$T/bad"; mkdir "$T/bad"; cp "$T/assets/"* "$T/bad/"
    cp "$T/expired.pub" "$T/bad/mindspace-release-signing-key.asc"
    for f in "$ARCHIVE" "$MANIFEST"; do
        gpg --homedir "$T/gk2" --batch --quiet --pinentry-mode loopback --passphrase '' --faked-system-time 20200101T120000 \
            --yes --armor --detach-sign -o "$T/bad/$f.asc" "$T/bad/$f"
    done
    cp "$INSTALL" "$T/inst-exp.sh"
    sed -i "s/^FPR=.*/FPR=$FP2/" "$T/inst-exp.sh"
    for f in "$ARCHIVE.asc" "$MANIFEST.asc" mindspace-release-signing-key.asc; do
        INST=$T/inst-exp.sh; repin "$T/bad" "$f"; cp "$T/inst-mod.sh" "$T/inst-exp.sh"
    done
    INST=$T/inst-exp.sh
    run_inst --assets-dir "$T/bad" --verify-only --trust-fingerprint "$FP2" --key-file "$T/expired.pub"
    expect "a signature by an expired key is refused" 1 "expired"
    INST=
else
    printf 'skip gpg cannot create a back-dated key here\n'
fi

# --------------------------------------------------------------------------- real installs
if [ "$WITH_BUILD" = 1 ]; then
    printf '\nReal installs (building; this takes a few minutes)\n'
    HN=b1; new_home b1
    mkdir -p "$H/.local/share/mindspace"
    printf 'existing database bytes' >"$H/.local/share/mindspace/mind.db"
    db_before=$(sha256sum "$H/.local/share/mindspace/mind.db" | awk '{print $1}')
    run_inst --trust-fingerprint "$FPR"
    expect "fresh install succeeds" 0 "Installed and verified"
    PFX=$H/.local/lib/mindspace/$CUR
    BIN=$PFX/bin/mind
    [ "$("$BIN" --version 2>/dev/null)" = "mind 0.1.0-alpha.1" ] && pass "installed binary reports mind 0.1.0-alpha.1" || fail "installed binary version"
    [ "$(stat -c %a "$BIN")" = 755 ] && [ "$(stat -c %a "$PFX")" = 700 ] && pass "install modes are 0755 binary, 0700 prefix" || fail "install modes"
    bin_sha=$(sha256sum "$BIN" | awk '{print $1}')
    if [ "$(stat -c %a "$PFX/install-record")" = 600 ] && grep -qx "version=$CUR" "$PFX/install-record" &&
        grep -qx "binary_sha256=$bin_sha" "$PFX/install-record" && grep -qx "archive_sha256=$PIN_ARCHIVE" "$PFX/install-record"; then
        pass "the install record describes the installed binary"
    else fail "install record"; fi
    [ "$(sha256sum "$H/.local/share/mindspace/mind.db" | awk '{print $1}')" = "$db_before" ] && pass "existing database is byte-identical" || fail "existing database changed"
    extra=$(cd "$H" && find . -mindepth 1 | while IFS= read -r f; do
        case "$f" in
            ./.local | ./.local/lib | ./.local/lib/mindspace | ./.local/lib/mindspace/0.1.0-alpha.1 | ./.local/lib/mindspace/0.1.0-alpha.1/bin | ./.local/lib/mindspace/0.1.0-alpha.1/bin/mind | ./.local/lib/mindspace/0.1.0-alpha.1/install-record) ;;
            ./.local/share | ./.local/share/mindspace | ./.local/share/mindspace/mind.db) ;;
            *) printf '%s\n' "$f" ;;
        esac
    done)
    [ -z "$extra" ] && pass "only the versioned prefix was created in HOME" || fail "unexpected files in HOME" "$extra"
    no_leftovers "fresh install"
    run_inst --trust-fingerprint "$FPR"
    expect "second run: already up to date, exit 0" 0 "Already up to date"
    [ "$(sha256sum "$BIN" | awk '{print $1}')" = "$bin_sha" ] && pass "binary unchanged by the second run" || fail "binary changed by the second run"

    # CLI quickstart with a fresh database, using absolute paths and --db (shell neutral)
    QDB=$H/mindspace-quickstart/mind.db
    mkdir -p "$H/repo"; (cd "$H/repo" && git init -q -b main && printf '# Retry policy\n\nWe retry failed uploads three times with backoff.\n' >decision.md &&
        printf 'pub fn parse_config() {}\n' >lib.rs && git add . && git -c user.name=t -c user.email=t@example.invalid -c commit.gpgsign=false commit -qm init)
    if "$BIN" --db "$QDB" init >/dev/null 2>&1 && "$BIN" --db "$QDB" space create myproject >/dev/null 2>&1 &&
        "$BIN" --db "$QDB" ingest "$H/repo" --space myproject >/dev/null 2>&1 &&
        "$BIN" --db "$QDB" search retry --space myproject 2>&1 | grep -q 'Retry policy' &&
        "$BIN" --db "$QDB" symbols parse_config --space myproject 2>&1 | grep -q 'parse_config'; then pass "quickstart: init, space, ingest, search, symbols"; else fail "quickstart commands"; fi
    mkdir -p "$H/.codex"
    cfg_before=$(tree_of "$H")
    HOME=$H CODEX_HOME=$H/.codex "$BIN" --db "$QDB" setup codex >"$T/setup.out" 2>&1
    if grep -qi 'preview' "$T/setup.out" && [ "$cfg_before" = "$(tree_of "$H")" ]; then pass "mind setup codex previews and changes nothing"; else fail "mind setup codex preview" "$(tail -n 5 "$T/setup.out")"; fi
    qdb_before=$(sha256sum "$QDB" | awk '{print $1}')
    printf 'sentinel codex MCP registration\n' >"$H/.codex/config.toml"
    mkdir -p "$H/.config/mindspace"
    printf 'sentinel application configuration\n' >"$H/.config/mindspace/config.toml"
    config_before=$(tree_of "$H/.config/mindspace"; tree_of "$H/.codex")

    # An install made without a record (an earlier installer) is reported, never silently replaced.
    rm "$PFX/install-record"
    run_inst --trust-fingerprint "$FPR"
    expect "an install without a record is reported and left alone" 0 "no install record"
    [ "$(sha256sum "$BIN" | awk '{print $1}')" = "$bin_sha" ] && pass "binary unchanged without a record" || fail "binary changed without a record"

    # Explicit reinstall, started from a Fish session when Fish is available.
    if command -v fish >/dev/null 2>&1; then
        OUT=$(env -i HOME="$H" PATH="$PATH" TMPDIR="$T/tmp-b1" RUSTUP_HOME="${RUSTUP_HOME:-$HOME/.rustup}" \
            fish -c "sh '$INSTALL' --version 0.1.0-alpha.1 --reinstall --trust-fingerprint $FPR" </dev/null 2>&1); RC=$?
    else
        run_inst --reinstall --trust-fingerprint "$FPR"
    fi
    expect "--reinstall replaces the install (from a fish session when available)" 0 "Installed and verified"
    prevs=$(ls "$H/.local/lib/mindspace/previous" 2>/dev/null | wc -l | tr -d ' ')
    prev=$(ls -d "$H/.local/lib/mindspace/previous"/* 2>/dev/null | head -n1)
    if [ "$prevs" = 1 ] && [ "$(sha256sum "$prev/install/bin/mind" | awk '{print $1}')" = "$bin_sha" ]; then pass "the previous binary was kept, byte for byte"; else fail "previous installation not kept" "$prevs entries in previous/"; fi
    [ -f "$PFX/install-record" ] && pass "the new install has a record" || fail "the new install has no record"
    [ "$(sha256sum "$H/.local/share/mindspace/mind.db" | awk '{print $1}')" = "$db_before" ] && [ "$(sha256sum "$QDB" | awk '{print $1}')" = "$qdb_before" ] && pass "databases are byte-identical after the reinstall" || fail "a database changed during the reinstall"
    [ "$config_before" = "$(tree_of "$H/.config/mindspace"; tree_of "$H/.codex")" ] && pass "configuration and MCP registrations are byte-identical" || fail "configuration or MCP registration changed"
    no_leftovers "reinstall"
    run_inst --trust-fingerprint "$FPR"
    expect "after the reinstall it is up to date again" 0 "Already up to date"
    new_sha=$(sha256sum "$BIN" | awk '{print $1}')

    # A reinstall that fails after the old install was moved aside puts the old one back.
    mkdir -p "$T/shim-install"
    printf '#!/bin/sh\nexit 1\n' >"$T/shim-install/install"; chmod +x "$T/shim-install/install"
    with_path "$T/shim-install:$PATH" run_inst --reinstall --trust-fingerprint "$FPR"
    expect "a staging failure leaves the current install untouched" 1 "cannot stage the binary"
    [ "$(sha256sum "$BIN" | awk '{print $1}')" = "$new_sha" ] && pass "staging failure preserved the binary" || fail "staging failure changed the binary"

    mkdir -p "$T/shim-publish"
    REAL_MV=$(command -v mv)
    cat >"$T/shim-publish/mv" <<EOF
#!/bin/sh
case "\${3:-}" in */.install.*) exit 1 ;; esac
exec "$REAL_MV" "\$@"
EOF
    chmod +x "$T/shim-publish/mv"
    with_path "$T/shim-publish:$PATH" run_inst --reinstall --trust-fingerprint "$FPR"
    expect "a publication failure restores the old install" 1 "restored"
    if [ "$(sha256sum "$BIN" | awk '{print $1}')" = "$new_sha" ] && [ -f "$PFX/install-record" ]; then pass "the previous installation was restored intact"; else fail "the previous installation was not restored"; fi
    [ "$(ls "$H/.local/lib/mindspace/previous" | wc -l | tr -d ' ')" = 1 ] && pass "no stray backup was left" || fail "stray backup left behind"
    no_leftovers "failed reinstall"

    # Interrupted reinstall: the existing install is untouched.
    env -i HOME="$H" PATH="$PATH" TMPDIR="$T/tmp-b1" RUSTUP_HOME="${RUSTUP_HOME:-$HOME/.rustup}" \
        sh "$INSTALL" --version 0.1.0-alpha.1 --reinstall --trust-fingerprint "$FPR" >"$T/int.out" 2>&1 </dev/null &
    ipid=$!
    i=0
    while [ $i -lt 120 ] && ! ls "$T/tmp-b1"/mindspace-install.*/build.log >/dev/null 2>&1; do sleep 1; i=$((i + 1)); done
    sleep 3
    kill -TERM "$ipid" 2>/dev/null
    wait "$ipid" 2>/dev/null
    sleep 2
    no_leftovers "interrupted reinstall"
    if [ "$(sha256sum "$BIN" | awk '{print $1}')" = "$new_sha" ] && [ "$(ls "$H/.local/lib/mindspace/previous" | wc -l | tr -d ' ')" = 1 ]; then pass "an interrupted reinstall leaves the existing install untouched"; else fail "interrupted reinstall changed the install"; fi

    # Interrupted first install: nothing is left.
    HN=int; new_home int
    env -i HOME="$H" PATH="$PATH" TMPDIR="$T/tmp-int" RUSTUP_HOME="${RUSTUP_HOME:-$HOME/.rustup}" \
        sh "$INSTALL" --version 0.1.0-alpha.1 --trust-fingerprint "$FPR" >"$T/int.out" 2>&1 </dev/null &
    ipid=$!
    i=0
    while [ $i -lt 120 ] && ! ls "$T/tmp-int"/mindspace-install.*/build.log >/dev/null 2>&1; do sleep 1; i=$((i + 1)); done
    sleep 3
    kill -TERM "$ipid" 2>/dev/null
    wait "$ipid" 2>/dev/null
    sleep 2
    no_leftovers "interrupted install"
    [ ! -e "$H/.local/lib/mindspace/0.1.0-alpha.1" ] && pass "interrupted install leaves no prefix" || fail "interrupted install left a prefix"
fi

printf '\n%s checks, %s failed\n' "$N" "$FAILS"
[ "$FAILS" = 0 ]
