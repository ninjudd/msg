#!/bin/sh
#
# Rasterize assets/msgd.svg into assets/msgd.icns.
#
# This is not part of the build. The .icns is committed and `pnpm build:msgd`
# only copies it; run this by hand after editing the SVG. That split is
# deliberate — an icon pipeline that ran during the build would be the one thing
# still requiring a runtime after docs/projects/all/rust-rewrite.md, so this uses
# nothing but what ships with macOS.
#
# `qlmanage` is the rasterizer. Nothing else here can do it: this machine has no
# rsvg-convert, inkscape, magick, cairosvg or resvg, and every one of those would
# be a dependency for a file that changes once a year. QuickLook renders SVG
# through WebKit, keeps the alpha channel, and honours -s exactly.
#
# Each size is rendered from the vector rather than downscaled from 1024, which
# is what keeps the chevron legible at 16 and 32 points — the sizes the Full Disk
# Access and Automation lists actually use.

set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
svg="$root/assets/msgd.svg"
icns="$root/assets/msgd.icns"

[ -f "$svg" ] || { echo "no SVG at $svg" >&2; exit 1; }

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
iconset="$work/msgd.iconset"
mkdir -p "$iconset"

# qlmanage always writes <basename>.png into the -o directory, so each render
# has to be moved out of the way before the next one overwrites it.
render() {
    rm -f "$work/msgd.svg.png"
    qlmanage -t -s "$1" -o "$work" "$svg" >/dev/null 2>&1 || true
    [ -f "$work/msgd.svg.png" ] || { echo "qlmanage rendered nothing at ${1}px" >&2; exit 1; }
    mv "$work/msgd.svg.png" "$iconset/$2"
}

render 16 icon_16x16.png
render 32 icon_16x16@2x.png
cp "$iconset/icon_16x16@2x.png" "$iconset/icon_32x32.png"
render 64 icon_32x32@2x.png
render 128 icon_128x128.png
render 256 icon_128x128@2x.png
cp "$iconset/icon_128x128@2x.png" "$iconset/icon_256x256.png"
render 512 icon_256x256@2x.png
cp "$iconset/icon_256x256@2x.png" "$iconset/icon_512x512.png"
render 1024 icon_512x512@2x.png

iconutil --convert icns "$iconset" --output "$icns"
echo "wrote $icns"
