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
        sh "${INST:-$INSTALL}" "$@" </dev/null 2>&1)
    RC=$?
}
with_path() { TEST_PATH=$1; shift; "$@"; TEST_PATH=; }
expect() { # NAME WANT_RC PATTERN
    if [ "$RC" = "$2" ] && printf '%s' "$OUT" | grep -q -- "$3"; then pass "$1"; else fail "$1 (rc=$RC, wanted $2 and /$3/)" "$(printf '%s' "$OUT" | tail -n 6)"; fi
}
tree_of() { (cd "$1" && find . | sort); }
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
    for tool in sh uname setsid pgrep sha256sum tar gzip gpg awk grep sed tr cmp mktemp install df wc curl cc gcc cut mv cp mkdir rmdir rm chmod cat head tail env dirname sleep cargo rustc rustup; do
        skip=0
        for o in "$@"; do [ "$o" = "$tool" ] && skip=1; done
        [ "$skip" = 1 ] && continue
        p=$(command -v "$tool" 2>/dev/null) && [ -x "$p" ] && ln -sf "$p" "$d/$tool"
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
HN=exist; new_home exist
mkdir -p "$H/.local/lib/mindspace/0.1.0-alpha.1/bin"
printf 'precious' >"$H/.local/lib/mindspace/0.1.0-alpha.1/bin/mind"
before=$(tree_of "$H")
run_inst --trust-fingerprint "$FPR" --assets-dir "$T/does-not-exist"
expect "existing prefix is refused before any download" 1 "already exists"
if [ "$(cat "$H/.local/lib/mindspace/0.1.0-alpha.1/bin/mind")" = precious ] && [ "$before" = "$(tree_of "$H")" ]; then pass "existing install is untouched"; else fail "existing install was modified"; fi
no_leftovers "existing prefix"

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
run_inst --verify-only --trust-fingerprint "$(printf '%s' "$FPR" | sed 's/\(....\)/\1 /g')"
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
OUT=$(env -i HOME="$H" PATH="$PATH" TMPDIR="$T/cc/tmp" sh "$INSTALL" --trust-fingerprint "$FPR" --assets-dir "$T/does-not-exist" </dev/null 2>&1); RC=$?
expect "a Cargo config above the build directory is refused before any work" 1 "Cargo would read"
OUT=$(env -i HOME="$H" PATH="$PATH" TMPDIR="$T/cc/tmp" sh "$INSTALL" --verify-only --trust-fingerprint "$FPR" --assets-dir "$T/assets" --key-file "$T/key.asc" </dev/null 2>&1); RC=$?
expect "the same directory is fine for --verify-only (nothing is built)" 0 "Authenticated"
HN=rel; new_home rel
mkdir -p "$T/rel/reltmp"
OUT=$(cd "$T/rel" && env -i HOME="$H" PATH="$PATH" TMPDIR=reltmp sh "$INSTALL" --verify-only --trust-fingerprint "$FPR" --assets-dir "$T/assets" --key-file "$T/key.asc" </dev/null 2>&1); RC=$?
expect "a relative TMPDIR is resolved" 0 "Authenticated"

# --------------------------------------------------------------------------- the typed fingerprint prompt
if command -v script >/dev/null 2>&1; then
    HN=pty; new_home pty
    tty_run() { # ANSWER: runs the installer on a pseudo terminal and types ANSWER
        OUT=$(printf '%s\n' "$1" | env -i HOME="$H" PATH="$PATH" TERM=dumb TMPDIR="$T/tmp-$HN" \
            script -qec "sh $INSTALL --assets-dir $T/assets --verify-only --key-file $T/key.asc" /dev/null 2>&1)
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
    BIN=$H/.local/lib/mindspace/0.1.0-alpha.1/bin/mind
    [ "$("$BIN" --version 2>/dev/null)" = "mind 0.1.0-alpha.1" ] && pass "installed binary reports mind 0.1.0-alpha.1" || fail "installed binary version"
    [ "$(stat -c %a "$BIN")" = 755 ] && [ "$(stat -c %a "$H/.local/lib/mindspace/0.1.0-alpha.1")" = 700 ] && pass "install modes are 0755 binary, 0700 prefix" || fail "install modes"
    [ "$(sha256sum "$H/.local/share/mindspace/mind.db" | awk '{print $1}')" = "$db_before" ] && pass "existing database is byte-identical" || fail "existing database changed"
    extra=$(cd "$H" && find . -mindepth 1 | while IFS= read -r f; do
        case "$f" in
            ./.local | ./.local/lib | ./.local/lib/mindspace | ./.local/lib/mindspace/0.1.0-alpha.1 | ./.local/lib/mindspace/0.1.0-alpha.1/bin | ./.local/lib/mindspace/0.1.0-alpha.1/bin/mind) ;;
            ./.local/share | ./.local/share/mindspace | ./.local/share/mindspace/mind.db) ;;
            *) printf '%s\n' "$f" ;;
        esac
    done)
    [ -z "$extra" ] && pass "only the versioned prefix was created in HOME" || fail "unexpected files in HOME" "$extra"
    no_leftovers "fresh install"
    bin_before=$(sha256sum "$BIN" | awk '{print $1}')
    run_inst --trust-fingerprint "$FPR"
    expect "second run is refused, not overwritten" 1 "already exists"
    [ "$(sha256sum "$BIN" | awk '{print $1}')" = "$bin_before" ] && pass "binary unchanged by the second run" || fail "binary changed by the second run"

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

    # Fish: the documented entry point is `sh install.sh` even from a Fish session.
    if command -v fish >/dev/null 2>&1; then
        HN=fish; new_home fish
        OUT=$(env -i HOME="$H" PATH="$PATH" TMPDIR="$T/tmp-fish" RUSTUP_HOME="${RUSTUP_HOME:-$HOME/.rustup}" \
            fish -c "sh '$INSTALL' --trust-fingerprint $FPR" </dev/null 2>&1); RC=$?
        expect "installer runs from a fish session" 0 "Installed and verified"
    fi

    # Interrupted install: stop it while it builds.
    HN=int; new_home int
    env -i HOME="$H" PATH="$PATH" TMPDIR="$T/tmp-int" RUSTUP_HOME="${RUSTUP_HOME:-$HOME/.rustup}" \
        sh "$INSTALL" --trust-fingerprint "$FPR" >"$T/int.out" 2>&1 </dev/null &
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
