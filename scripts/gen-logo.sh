#!/usr/bin/env bash
# Regenerates the logo (assets/logo) from the geometry and palettes below.
# The mark is one solid message bubble with two outlined copies fanned out
# behind it: one logical message, many platform copies.
# SVGs are written by this script; the PNG/ICO exports need ImageMagick built
# with librsvg (`magick`).
set -euo pipefail
cd "$(dirname "$0")/../assets/logo"

# Palettes, named for the background they sit on. on-dark matches docs/index.html.
declare -A on_dark=([message]='#7c9cff' [copies]='#57d9b3' [text]='#e7ebf4')
declare -A on_light=([message]='#4c6ef5' [copies]='#0ca678' [text]='#0c0e14')
icon_bg='#0c0e14'

# The bubble, on a 128x128 grid; the copies are the same bubble offset by copy_step.
bubble='M32 40H68A18 18 0 0 1 86 58V78A18 18 0 0 1 68 96H44L20 110C22 105 14 99 14 84V58A18 18 0 0 1 32 40Z'
copy_step=13

# Monoline lowercase glyphs: stroke centerlines, x-height 40..80, ascender 20,
# descender 100, stroke 10 with round caps. "<x offset>|<path>" per glyph.
glyph_w='M0 40V68a12 12 0 0 0 24 0a12 12 0 0 0 24 0V40M24 52V68'
glyphs=(
  "0|$glyph_w"
  "68|M0 40V62a18 18 0 0 0 36 0V40M36 40V80"
  "124|M0 40V100M0 60a20 20 0 1 0 40 0a20 20 0 1 0 -40 0"
  "184|M0 20V80M0 58a18 18 0 0 1 36 0V80"
  "240|M10 80V36a16 16 0 0 1 16-16h4M0 40H26"
)

# mark <palette-name>: the mark's <defs> and body on the 128x128 grid.
mark() {
  local -n pal=$1
  local far=$((copy_step * 2))
  cat <<EOF
  <defs>
    <path id="bubble" d="$bubble"/>
    <mask id="gap-far" maskUnits="userSpaceOnUse" x="0" y="0" width="128" height="128">
      <rect width="128" height="128" fill="#fff"/>
      <use href="#bubble" transform="translate($copy_step -$copy_step)" stroke="#000" stroke-width="9" stroke-linejoin="round"/>
    </mask>
    <mask id="gap-near" maskUnits="userSpaceOnUse" x="0" y="0" width="128" height="128">
      <rect width="128" height="128" fill="#fff"/>
      <use href="#bubble" stroke="#000" stroke-width="9" stroke-linejoin="round"/>
    </mask>
    <mask id="initial" maskUnits="userSpaceOnUse" x="0" y="0" width="128" height="128">
      <rect width="128" height="128" fill="#fff"/>
      <path d="$glyph_w" transform="translate(34 29) scale(.66)" fill="none" stroke="#000" stroke-width="10" stroke-linecap="round" stroke-linejoin="round"/>
    </mask>
  </defs>
  <g transform="translate(2 0)">
    <!-- Masks sit on wrapping groups: on a transformed <use> they would move with it. -->
    <g mask="url(#gap-far)"><use href="#bubble" transform="translate($far -$far)" fill="none" stroke="${pal[copies]}" stroke-opacity=".45" stroke-width="6" stroke-linejoin="round"/></g>
    <g mask="url(#gap-near)"><use href="#bubble" transform="translate($copy_step -$copy_step)" fill="none" stroke="${pal[copies]}" stroke-width="6" stroke-linejoin="round"/></g>
    <use href="#bubble" mask="url(#initial)" fill="${pal[message]}"/>
  </g>
EOF
}

# wordmark <palette-name> <x> <y>: "wuphf" with its glyph origin at x,y.
wordmark() {
  local -n pal=$1
  echo "  <g transform=\"translate($2 $3)\" fill=\"none\" stroke=\"${pal[text]}\" stroke-width=\"10\" stroke-linecap=\"round\" stroke-linejoin=\"round\">"
  local g
  for g in "${glyphs[@]}"; do
    echo "    <path transform=\"translate(${g%%|*} 0)\" d=\"${g#*|}\"/>"
  done
  echo "  </g>"
}

svg_open() { echo "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"$1\" width=\"$2\" height=\"$3\">"; echo "  <title>wuphf</title>"; }

for bg in dark light; do
  {
    svg_open '0 0 128 128' 128 128
    mark "on_$bg"
    echo '</svg>'
  } >"mark-on-$bg.svg"
  # Lockup: the wordmark right of the mark, nudged down so its x-height band
  # lines up with the front bubble.
  {
    svg_open '8 4 420 120' 420 120
    mark "on_$bg"
    wordmark "on_$bg" 144 4
    echo '</svg>'
  } >"logo-on-$bg.svg"
done

{
  svg_open '0 0 128 128' 128 128
  echo "  <rect width=\"128\" height=\"128\" rx=\"28\" fill=\"$icon_bg\"/>"
  echo '  <g transform="translate(64 64) scale(.8) translate(-64 -63)">'
  mark on_dark
  echo '  </g>'
  echo '</svg>'
} >icon.svg

command -v magick >/dev/null || { echo "magick not found: SVGs written, PNG/ICO exports skipped" >&2; exit 0; }
# No timestamps in the PNGs, so regenerating unchanged art leaves no diff.
png_opts=(-define png:exclude-chunks=date,time)
for size in 512 192; do
  magick -background none -density $((size * 72 / 128)) icon.svg "${png_opts[@]}" "icon-$size.png"
done
magick -background none -density 576 icon.svg -define icon:auto-resize=48,32,16 favicon.ico
for bg in dark light; do
  magick -background none -density 288 "logo-on-$bg.svg" "${png_opts[@]}" "logo-on-$bg.png"
done
# GitHub social preview (1280x640): the lockup centred on the icon background.
magick -size 1280x640 "xc:$icon_bg" \( -background none -density 576 logo-on-dark.svg -resize 800x \) -gravity center -composite "${png_opts[@]}" social-preview.png
