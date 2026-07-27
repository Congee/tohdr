#!/bin/sh
# Install tohdr.lrplugin into Lightroom Classic's Modules folder, stamping the
# commit it was built from into Info.lua's VERSION.build.
#
# Why the stamp: the Modules folder is scanned only at launch, and a running
# Lightroom holds the .lua files it loaded at startup. Nothing inside the plugin
# can tell you which copy is live -- access times on those files do not move when
# Lightroom reads them, which is a trap worth naming because it cost an hour of
# debugging once. Plug-in Manager displays the VERSION, so after this the answer
# to "is Lightroom running what I just built?" is: look at it.
#
# Two halves, because one number cannot do both jobs. `VERSION.build` gets
# `YYMMDDHHMM` in UTC -- a *number*, since Plug-in Manager will not render a
# string as the fourth component of `0.1.0.x`, and time-based so it rises on
# every install including a reinstall of a dirty tree. The git hash goes on the
# `installed-from` comment line, where it stays exact.
#
# Usage: tools/install-lrplugin.sh [dest-dir]
#
# Idempotent: run it again after any change. Restart Lightroom afterwards, or the
# .lua half of what you just installed will not be the half that runs.
set -eu

REPO="$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)"
SRC="$REPO/lightroom/tohdr.lrplugin"
DEST="${1:-$HOME/Library/Application Support/Adobe/Lightroom/Modules/tohdr.lrplugin}"

[ -f "$SRC/Info.lua" ] || { echo "no plugin at $SRC" >&2; exit 1; }

# `git -C` rather than a cd, so this works from anywhere. A dirty tree is marked:
# a stamp that names a commit the files do not match is worse than no stamp.
if SHA="$(git -C "$REPO" rev-parse --short=8 HEAD 2>/dev/null)"; then
    git -C "$REPO" diff --quiet HEAD -- lightroom crates || SHA="$SHA-dirty"
else
    SHA="nogit"
fi
BUILD="$(date -u '+%y%m%d%H%M')"
STAMP="$SHA ($(date -u '+%Y-%m-%dT%H:%MZ'))"

echo "building tohdr (release)"
( cd "$REPO" && cargo build --release -p tohdr-cli )

mkdir -p "$DEST"
for f in "$SRC"/*.lua; do
    install -m 644 "$f" "$DEST/"
done
install -m 755 "$REPO/target/release/tohdr" "$DEST/tohdr"

# Rewrite the build field and the provenance line, in the installed copy only --
# never in the checkout, which stays `build = 0` so a stamp cannot be committed
# by accident.
INFO="$DEST/Info.lua"
TMP="$INFO.tmp.$$"
sed -e "s/build = [0-9]*/build = $BUILD/" \
    -e "s|-- installed-from: .*|-- installed-from: $STAMP|" "$INFO" > "$TMP"
grep -q "build = $BUILD" "$TMP" || { rm -f "$TMP"; echo "stamp failed" >&2; exit 1; }
grep -q "installed-from: $SHA" "$TMP" || { rm -f "$TMP"; echo "provenance failed" >&2; exit 1; }
mv "$TMP" "$INFO"
chmod 644 "$INFO"

# The whole version, the way Plug-in Manager shows it, so what to look for on
# screen needs no assembling.
VERSION="$(sed -n 's/.*major = \([0-9]*\), minor = \([0-9]*\), revision = \([0-9]*\), build = \([0-9]*\).*/\1.\2.\3.\4/p' "$INFO")"

echo "installed to $DEST"
echo "  version:       $VERSION"
echo "  installed from: $STAMP"
echo "  tohdr:         $("$DEST/tohdr" --version 2>/dev/null || echo '(no --version)')"
echo
echo "Restart Lightroom -- the Modules folder is only scanned at launch."
