#!/bin/sh
# Install tohdr.lrplugin into Lightroom Classic's Modules folder, stamping the
# commit it was built from into Info.lua's VERSION.build.
#
# Why the stamp: the Modules folder is scanned only at launch, and a running
# Lightroom holds the .lua files it loaded at startup. Nothing inside the plugin
# can tell you which copy is live -- access times on those files do not move when
# Lightroom reads them, which is a trap worth naming because it cost an hour of
# debugging once. Plug-in Manager displays the VERSION string, so after this the
# answer to "is Lightroom running what I just built?" is: look at it.
#
# The format is Adobe's own, read off their LrC 15.3 samples, which all carry
# `build="202604090947-8f3672ed"` -- a UTC timestamp and a git short hash.
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
STAMP="$(date -u '+%Y%m%d%H%M')-$SHA"

echo "building tohdr (release)"
( cd "$REPO" && cargo build --release -p tohdr-cli )

mkdir -p "$DEST"
for f in "$SRC"/*.lua; do
    install -m 644 "$f" "$DEST/"
done
install -m 755 "$REPO/target/release/tohdr" "$DEST/tohdr"

# Rewrite only the build field, in the installed copy -- never in the checkout,
# which stays `"dev"` so the stamp can never be committed by accident.
INFO="$DEST/Info.lua"
TMP="$INFO.tmp.$$"
sed "s/build = \"[^\"]*\"/build = \"$STAMP\"/" "$INFO" > "$TMP"
grep -q "build = \"$STAMP\"" "$TMP" || { rm -f "$TMP"; echo "stamp failed" >&2; exit 1; }
mv "$TMP" "$INFO"
chmod 644 "$INFO"

echo "installed to $DEST"
echo "  version build: $STAMP"
echo "  tohdr:         $("$DEST/tohdr" --version 2>/dev/null || echo '(no --version)')"
echo
echo "Restart Lightroom -- the Modules folder is only scanned at launch."
