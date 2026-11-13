#!/bin/sh
# Offline filesystem transaction tests: execute the installer's actual helpers and publication tail.
# A tiny executable fixture replaces the download/build phases. These tests prove publication,
# failure, signal and lock behavior; the signed-release --with-build suite proves real builds.
set -eu
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
T=$(mktemp -d /tmp/mindspace-publish.XXXXXX)
trap 'rm -rf -- "$T"' EXIT
CUR=0.1.0-alpha.1
N=0
FAILS=0
python3 - "$ROOT/install.sh" "$T/publish.sh" <<'PY'
from pathlib import Path
import sys
s=Path(sys.argv[1]).read_text()
header=s[:s.index('# ---------------------------------------------------------------- arguments')]
a=s.index('ver_cmp() {'); b=s.index('if [ "$VERIFY_ONLY" != 1 ]; then check_existing; fi',a)
tail=s[s.index('# ---------------------------------------------------------------- 7. install'):]
setup='''
INSTALL_ROOT=$HOME/.local/lib/mindspace
PREFIX=$INSTALL_ROOT/$VERSION
BIN=$TEST_BINARY
REINSTALL=${TEST_REINSTALL:-0}
UPGRADE=${TEST_UPGRADE:-0}
ALLOW_OLDER=${TEST_ALLOW_OLDER:-0}
sha_of() { sha256sum "$1" | awk '{print $1}'; }
'''
Path(sys.argv[2]).write_text(header+s[a:b]+setup+tail)
PY
printf '#!/bin/sh\necho mind %s\n' "$CUR" >"$T/mind"
chmod 755 "$T/mind"
new_case() { H=$T/$1; mkdir -p "$H"; TEST_PATH=$PATH; RE=0; UP=0; AO=0; }
run() {
    RC=0
    OUT=$(env -i HOME="$H" PATH="$TEST_PATH" TEST_BINARY="$T/mind" TEST_REINSTALL="$RE" \
        TEST_UPGRADE="$UP" TEST_ALLOW_OLDER="$AO" timeout 10 sh "$T/publish.sh" 2>&1) || RC=$?
}
check() {
    N=$((N+1))
    if [ "$RC" = "$2" ] && printf '%s\n' "$OUT" | grep -qF -- "$3"; then printf 'ok   %s\n' "$1"
    else FAILS=$((FAILS+1)); printf 'FAIL %s (rc=%s expected=%s text=%s)\n%s\n' "$1" "$RC" "$2" "$3" "$OUT"; fi
}
snapshot() { (cd "$1" && find . -printf '%p %y %m %s %T@ %l\n' | sort; find . -type f -exec sha256sum {} + | sort); }
assert_same() { RC=0; OUT=preserved; [ "$before" = "$(snapshot "$H")" ] || { RC=1; OUT=changed; }; check "$1" 0 preserved; }
legacy() { p=$H/.local/lib/mindspace/$1; mkdir -p "$p/bin"; printf '#!/bin/sh\necho mind %s\n' "$1" >"$p/bin/mind"; chmod 755 "$p/bin/mind"; }
new_case normal
run; check 'fresh staged publication succeeds' 0 'Installed and verified'
before=$(snapshot "$H"); run; check 'repeat publication is an untouched no-op' 0 'Already up to date'; assert_same 'repeat preserves bytes and metadata'
p=$H/.local/lib/mindspace/$CUR
old_sum=$(sha256sum "$p/bin/mind" | awk '{print $1}')
RE=1; run; check 'explicit reinstall publishes' 0 'Installed and verified'
backup=$(find "$H/.local/lib/mindspace/previous" -type f -name mind)
RC=0; OUT=preserved; [ "$(sha256sum "$backup" | awk '{print $1}')" = "$old_sum" ] || { RC=1; OUT=changed; }
check 'old binary remains byte-identical in backup' 0 preserved

REAL_INSTALL=$(command -v install)
REAL_MV=$(command -v mv)
mkdir "$T/fail-stage" "$T/fail-publish" "$T/signal-move" "$T/appeared"
printf '#!/bin/sh\nexit 1\n' >"$T/fail-stage/install"
cat >"$T/fail-publish/mv" <<EOF2
#!/bin/sh
case "\${3:-}" in */.install.*) exit 1 ;; esac
exec "$REAL_MV" "\$@"
EOF2
cat >"$T/signal-move/mv" <<EOF2
#!/bin/sh
"$REAL_MV" "\$@" || exit 1
case "\${3:-}" in */$CUR) kill -s TERM "\$PPID" ;; esac
EOF2
cat >"$T/appeared/install" <<EOF2
#!/bin/sh
"$REAL_INSTALL" "\$@" || exit 1
mkdir -p "\$HOME/.local/lib/mindspace/$CUR"
printf precious >"\$HOME/.local/lib/mindspace/$CUR/precious"
EOF2
chmod 755 "$T/fail-stage/install" "$T/fail-publish/mv" "$T/signal-move/mv" "$T/appeared/install"
before=$(snapshot "$p"; snapshot "$H/.local/lib/mindspace/previous"); TEST_PATH=$T/fail-stage:$PATH
run; check 'stage copy failure reports safe failure' 1 'cannot stage the binary'
RC=0; OUT=preserved
[ "$before" = "$(snapshot "$p"; snapshot "$H/.local/lib/mindspace/previous")" ] || { RC=1; OUT=changed; }
check 'stage failure preserves install and backup bytes and metadata' 0 preserved
# A failed rename changes directory mtimes; compare all application files, not transaction-parent metadata.
TEST_PATH=$T/fail-publish:$PATH
run; check 'publication failure restores previous prefix' 1 'previous installation was restored'
RC=0; OUT=preserved; [ "$(sha256sum "$p/bin/mind" | awk '{print $1}')" = "$old_sum" ] || { RC=1; OUT=changed; }
check 'rollback restored previous binary bytes' 0 preserved
TEST_PATH=$T/signal-move:$PATH
run; check 'TERM immediately after old-prefix rename restores it' 143 'previous installation was restored'
RC=0; OUT=preserved; [ "$(sha256sum "$p/bin/mind" | awk '{print $1}')" = "$old_sum" ] || { RC=1; OUT=changed; }
check 'signal rollback preserves previous binary' 0 preserved
TEST_PATH=$PATH
before=$(snapshot "$H")
exec 8<"$H/.local/lib/mindspace"
flock -n 8
run; check 'concurrent publisher fails without touching install' 1 'another installer is publishing'; assert_same 'held publication lock preserves install'
flock -u 8
exec 8<&-

# Interruption after replacement retains two complete binaries; a retry is a no-op.
new_case after_replace; legacy "$CUR"
mkdir "$T/signal-publish" "$T/late-data" "$T/late-empty"
cat >"$T/signal-publish/mv" <<EOF2
#!/bin/sh
"$REAL_MV" "\$@" || exit 1
case "\${3:-}" in */.install.*) kill -s TERM "\$PPID" ;; esac
EOF2
cat >"$T/late-data/install" <<EOF2
#!/bin/sh
"$REAL_INSTALL" "\$@" || exit 1
printf precious >"\$HOME/.local/lib/mindspace/$CUR/mind.db"
EOF2
cat >"$T/late-empty/mv" <<EOF2
#!/bin/sh
case "\$*" in *'/.install.'*) mkdir -p "\$HOME/.local/lib/mindspace/$CUR" ;; esac
exec "$REAL_MV" "\$@"
EOF2
chmod 755 "$T/signal-publish/mv" "$T/late-data/install" "$T/late-empty/mv"
RE=1; TEST_PATH=$T/signal-publish:$PATH; run
check 'TERM immediately after replacement preserves complete new prefix' 143 'previous installation is kept'
RC=0; OUT=preserved
[ "$("$H/.local/lib/mindspace/$CUR/bin/mind" --version)" = "mind $CUR" ] &&
    [ -f "$H/.local/lib/mindspace/$CUR/install-record" ] &&
    [ "$(find "$H/.local/lib/mindspace/previous" -name mind -type f | wc -l)" -eq 1 ] || { RC=1; OUT=changed; }
check 'post-replacement interruption keeps previous binary and receipt' 0 preserved
RE=0; TEST_PATH=$PATH; run; check 'retry after post-replacement interruption is safe' 0 'Already up to date'
new_case stage_data; legacy "$CUR"; RE=1; TEST_PATH=$T/late-data:$PATH
run; check 'data appearing during staging blocks backup rename' 1 'unexpected entry'
RC=0; OUT=preserved
[ "$(cat "$H/.local/lib/mindspace/$CUR/mind.db" 2>/dev/null)" = precious ] || { RC=1; OUT=changed; }
check 'data created during staging remains at original path' 0 preserved
new_case empty_race; TEST_PATH=$T/late-empty:$PATH
run; check 'late empty destination is never replaced' 1 'appeared meanwhile'

new_case late; TEST_PATH=$T/appeared:$PATH
run; check 'late destination is never overwritten or nested into' 1 'appeared meanwhile'
RC=0; OUT=preserved; [ "$(cat "$H/.local/lib/mindspace/$CUR/precious")" = precious ] || { RC=1; OUT=changed; }
check 'late destination user content survives' 0 preserved

new_case upgrade; legacy 0.0.9
before=$(snapshot "$H"); run; check 'publication rechecks explicit upgrade consent' 1 '--upgrade'; assert_same 'refused upgrade preserves old installation'
UP=1; run; check 'explicit upgrade publishes side by side' 0 'Installed and verified'
RC=0; OUT=preserved; [ "$("$H/.local/lib/mindspace/0.0.9/bin/mind" --version)" = 'mind 0.0.9' ] || { RC=1; OUT=changed; }
check 'older executable remains available' 0 preserved

new_case downgrade; legacy 0.2.0
before=$(snapshot "$H"); UP=1; RE=1; run; check 'publication refuses automatic downgrade' 1 'a newer version is already installed'; assert_same 'refused downgrade preserves newer installation'
AO=1; run; check 'explicit allow-older keeps both versions' 0 'Installed and verified'

new_case backup_link; legacy "$CUR"; mkdir "$H/external"; ln -s "$H/external" "$H/.local/lib/mindspace/previous"
RE=1; run; check 'linked backup directory is rejected' 1 'not a real directory'
RC=0; OUT=preserved
[ -x "$H/.local/lib/mindspace/$CUR/bin/mind" ] && [ -z "$(ls -A "$H/external")" ] || { RC=1; OUT=changed; }
check 'linked backup refusal preserves old binary and external directory' 0 preserved

left=$(find "$T" -type d -name '.install.*' -print)
RC=0; OUT=clean; [ -z "$left" ] || { RC=1; OUT=$left; }
check 'all transaction staging directories are cleaned' 0 clean
printf '\n%s publication checks, %s failed\n' "$N" "$FAILS"
[ "$FAILS" = 0 ]
