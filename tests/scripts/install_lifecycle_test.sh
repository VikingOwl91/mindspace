#!/bin/sh
# Offline lifecycle regressions. No download, build, or host configuration changes.
set -eu
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
T=$(mktemp -d /tmp/mindspace-lifecycle.XXXXXX)
trap 'rm -rf -- "$T"' EXIT
FPR=0E6699CB48F3941AE28CA88D509F78007F62D5EA
CUR=0.1.0-alpha.1
ARCHIVE_SHA=b587a04edff66963162267fff805678b7c1ba8aefbaec795e518702e0e4149c0
N=0
FAILS=0
new_case() { H=$T/$1; mkdir -p "$H"; }
fixture() {
    p=$H/.local/lib/mindspace/$1
    mkdir -p "$p/bin"
    printf '#!/bin/sh\nprintf "mind %s\\n"\n' "$1" >"$p/bin/mind"
    chmod 755 "$p/bin/mind"
    printf 'mindspace-install-record 1\nversion=%s\nbinary_sha256=%s\narchive_sha256=%s\nsource_commit=a027d94680392648b6277dc2fd9bc20906114b91\npublisher_fingerprint=%s\ninstalled_at=2026-11-13T12:00:00Z\n' \
        "$1" "$(sha256sum "$p/bin/mind" | awk '{print $1}')" "$ARCHIVE_SHA" "$FPR" >"$p/install-record"
}
run() {
    RC=0
    OUT=$(env -i HOME="$H" PATH="$PATH" TMPDIR="$T" RUSTUP_HOME="${RUSTUP_HOME:-$HOME/.rustup}" \
        timeout 20 sh "$ROOT/install.sh" --version "$CUR" --assets-dir "$T/missing" --trust-fingerprint "$FPR" "$@" 2>&1) || RC=$?
}
check() {
    N=$((N + 1))
    if [ "$RC" = "$2" ] && printf '%s\n' "$OUT" | grep -qF -- "$3"; then
        printf 'ok   %s\n' "$1"
    else
        FAILS=$((FAILS + 1)); printf 'FAIL %s (rc=%s, expected %s, text=%s)\n%s\n' "$1" "$RC" "$2" "$3" "$OUT"
    fi
}
new_case binlink; fixture "$CUR"
mv "$p/bin" "$H/realbin"; ln -s "$H/realbin" "$p/bin"
run; check 'linked bin directory is rejected' 1 'bin is not a real directory'
new_case fifo; fixture "$CUR"
rm "$p/install-record"; mkfifo "$p/install-record"
run; check 'FIFO record fails without hanging' 1 'install-record is not a regular file'
new_case duplicate; fixture "$CUR"
printf 'version=%s\n' "$CUR" >>"$p/install-record"
run; check 'duplicate receipt fields are invalid' 1 'install-record has an unknown format'
new_case identity; fixture "$CUR"
sed -i 's/publisher_fingerprint=.*/publisher_fingerprint=wrong/' "$p/install-record"
run; check 'incorrect recorded publisher is invalid' 1 'different version or build'
new_case corrupt; fixture "$CUR"
printf '#!/bin/sh\ntouch "%s/executed"\nprintf "mind %s\\n"\n' "$H" "$CUR" >"$p/bin/mind"
run; check 'hash mismatch detected before running binary' 1 'no longer matches'
RC=0; OUT='not executed'; [ ! -e "$H/executed" ] || { RC=1; OUT=executed; }
check 'damaged binary was never executed' 0 'not executed'
new_case unexpected; fixture "$CUR"; printf 'user data' >"$p/mind.db"
run --reinstall; check 'unexpected prefix data stays in place even with reinstall' 1 'unexpected entry'
new_case incomplete_data; mkdir -p "$H/.local/lib/mindspace/$CUR"
printf 'user data' >"$H/.local/lib/mindspace/$CUR/mind.db"
run --reinstall; check 'incomplete prefix data cannot be moved by reinstall' 1 'unexpected entry'
new_case nonexec_data; fixture "$CUR"; chmod 644 "$p/bin/mind"; printf 'data' >"$p/bin/mind.db"
run --reinstall; check 'nonexecutable binary cannot bypass data protection' 1 'unexpected entry'
new_case valid_bad_sibling; fixture "$CUR"; mkdir "$H/.local/lib/mindspace/0.2.0"
run; check 'intact current install is a no-op despite incomplete sibling' 0 'Already up to date'
new_case binary_directory; fixture "$CUR"; rm "$p/bin/mind"; mkdir "$p/bin/mind"; printf data >"$p/bin/mind/mind.db"
run --reinstall; check 'data in a directory at the binary slot stays in place' 1 'unexpected directory'
new_case receipt_directory; fixture "$CUR"; rm "$p/install-record"; mkdir "$p/install-record"; printf data >"$p/install-record/mind.db"
run --reinstall; check 'data in a directory at the receipt slot stays in place' 1 'unexpected directory'
new_case parentlink; mkdir -p "$H/elsewhere"; ln -s "$H/elsewhere" "$H/.local"
run --reinstall; check 'linked install ancestors are rejected' 1 'symbolic link'
new_case upgrade; fixture 0.0.9
run; check 'older installation needs explicit upgrade' 1 '--upgrade'
run --upgrade; check 'explicit upgrade passes version-transition consent without requiring Rust' 1 'Older versions installed: 0.0.9'
RC=0
if printf '%s\n' "$OUT" | grep -qF 'an older installation exists'; then RC=1; fi
OUT=authorized
check 'explicit upgrade no longer asks for consent' 0 authorized
new_case invalidother; mkdir -p "$H/.local/lib/mindspace/0.2.0"
run; check 'invalid sibling gets recovery guidance' 1 'not a valid installation'
new_case impostor; fixture 0.2.0
printf '#!/bin/sh\necho mind 0.0.9\n' >"$p/bin/mind"
run; check 'sibling name alone is not version proof' 1 'not a valid installation'
new_case hyphen; fixture 0.1.0-alpha-next
run; check 'hyphenated prerelease prevents downgrade' 1 'a newer version is already installed'
new_case downgrade; fixture 0.1.0
run --upgrade --reinstall; check 'upgrade plus reinstall never silently downgrades' 1 'a newer version is already installed'
new_case legacy; fixture "$CUR"; rm "$p/install-record"
run; check 'legacy installation repeat succeeds with caveat' 0 'Already up to date'
printf '\n%s lifecycle checks, %s failed\n' "$N" "$FAILS"
[ "$FAILS" = 0 ]
