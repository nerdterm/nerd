# Brand

Every graphic nerd owns. A file name says the role, the theme, and — where it
matters — the size the drawing is tuned for (`-16px`) or the pixels it holds
(`-256`, `-1600x375`). Braces below stand for the files that exist: both
themes, every listed size.

## The files

| File | What it is | Use it for |
| --- | --- | --- |
| `nerd-logo-{light,dark}.svg` | Full lockup: globe, wordmark, both taglines. Vector, 896×404 canvas | READMEs, docs, slides, anything a vector is taken in |
| `nerd-logo-{light,dark}-1792x808.png` | The lockup at twice its canvas, transparent | The same, where a vector is not taken |
| `nerd-wordmark-{light,dark}.svg` | NERD alone, no globe. Vector | Headers too small or too narrow for the globe |
| `nerd-wordmark-{light,dark}-1600x375.png` | Wordmark raster, transparent | **Embedded in the binary** by `src/ui.rs`: the sidebar draws it one row high |
| `nerd-appicon-{light,dark}.svg` | App icon tile, full grid. Vector | Any icon at 64 px and above |
| `nerd-appicon-{light,dark}-32px.svg` | Tile drawn for 32 px: bigger globe, one inner meridian, the equator | Favicons and lists at 24–48 px |
| `nerd-appicon-{light,dark}-16px.svg` | Tile drawn for 16 px: rim and N only | Anything at 16–23 px |
| `nerd-appicon-{light,dark}-{16,32,64,128,256,512,1024}.png` | The tile rastered, each size drawn from the band it belongs to | GitHub, package pages, favicons, anywhere a PNG is asked for |
| `nerd-appicon-{light,dark}.icns` | macOS icon file, 16→1024 including @2x. **Built from the Icon Composer artwork**, not from the tiles above — `node scripts/dock-icns.mjs` | The dock icon of a terminal, see below; Finder's *Get Info* |
| `icon-composer/nerd-icon-{light,dark}-1024.png` | The approved app icon, authored in Icon Composer: shape, shadow and glass already in the file | The source the two `.icns` are built from |
| `icon-composer/nerd-layer-{light,dark}.svg` | The drawing on its own, no ground and no shape | What goes back into Icon Composer to author it again |

No `.ico`: that is a Windows format and nerd is macOS only.

## Three drawings, one icon

An icon shrunk to 16 px turns to mush, so the tile is drawn three times. Pick by
the size it will be seen at, not by the file that is nearest to hand.

```
  >= 64 px          32 px band             16 px band
  4 meridians       bigger globe           rim and N only
  3 parallels       1 inner meridian       no grid
  edge lines cut    the equator            thickest strokes
```

The grid drops the line nearest the rim on each axis. Meridians sit at equal
longitudes, so on a flattened globe they bunch against the rim; without those
two lines the edge stops reading as noise. The 32 px and 16 px drawings are
already thinned and lose nothing further.

Every tile leaves the transparent margin macOS icons are drawn with — Apple's
grid, 824 of a 1024 canvas — so it stands beside other dock icons at the same
size instead of a quarter larger. The shadow lives in that margin, so what the
eye sees is wider than the body: 824 solid and 862 in all on a 1024 canvas,
against Apple's own 824 and 864. The small bands land on the same footprint
Apple's own slices have — 24 of 32 solid inside 26, and 12 of 16 inside 14.

The two `.icns` stand outside all of this. They are not drawn by
`scripts/brand.mjs` at all: they are built from the Icon Composer artwork in
`brand/icon-composer`, placed at 899 of 1024, the size that was checked in a real
dock. Nothing in this section applies to them.

## The shape

The tile is not a rounded rectangle. macOS draws app icons on a continuous curve
whose corners blend into the sides, and it masks every app bundle's icon with it:
a plain red square in an `.icns` comes back from `NSWorkspace.icon(forFile:)` the
same 824-wide shape as Calculator, Mail and Ghostty, to the pixel. The tile in
`scripts/brand.mjs` is that mask, fitted to it — 0.38 px rms and 2.5 px worst
over the 412 rows of one corner, where the `rx="45"` it replaced was 12.15 px rms
and 75 px worst. The shape is ours to get right wherever the file is used raw and
the system mask never runs: GitHub, docs, a custom terminal icon.

## The shadow and the rim

Both are drawn into the file, because nothing draws them for us. macOS decorates
an *application bundle's* icon; nerd has no bundle, and a raw image handed to
`NSApplication.icon` — which is all ghostty's `macos-custom-icon` does, and all
GitHub and these docs do — is drawn exactly as given. Apple bakes the same two
into the files it ships: `Calculator.app`, `Notes.app` and `Mail.app` all carry a
shadow skirt outside the body and a light rim inside it, identical to each other
to the alpha level.

The generator matches that skirt within 4 levels of 255 on every edge, and the
rim to +30 luminance at the body edge against Apple's +29. Both are set in tile
units so they follow the band; only the blur is widened for the small bands,
where a shadow under half a pixel wide would not survive the raster.

## Colours

| | Ground | Grid | Letters |
| --- | --- | --- | --- |
| Light | none, the file is transparent | `#141414` | `#141414` |
| Dark | none | `#9FD08A`, the accent | `#D6D8E2` |
| Icon tile, light | `#F3F3EF` | `#141414` | `#141414` |
| Icon tile, dark | `#16171D`, the app ground | `#9FD08A` | `#D6D8E2` |

The logo and the wordmark are transparent, so they take the ground they are put
on; only the icon carries its own tile. The grid holds the accent, the letters
never do. Over a photo or a busy ground, use the icon rather than the bare logo.

## The dock icon

nerd runs inside a terminal, so the dock tile belongs to that terminal, not to
nerd. Ghostty can wear ours, by hand — nerd has no setting for it, because the
tile belongs to the Ghostty *application*: macOS gives one per app and Ghostty
has no per-window icon, so the change reaches every Ghostty window, including
ones nerd was never started in. Two lines in `~/.config/ghostty/config`:

```
macos-icon = custom
macos-custom-icon = /absolute/path/to/nerd/brand/nerd-appicon-dark.icns
```

It accepts PNG, JPEG and ICNS, and the path must be absolute. Give it the
`.icns`: the 1024 PNG keeps Apple's grid and comes out smaller in the tile.
Ghostty puts the image into the tile through its own dock tile plugin, so
nothing masks it, nothing decorates it, and the file carries its own size. This
sets `NSApplication.icon`, so it changes the running app's dock tile; the icon
in Finder and in Ghostty's update dialog stays Ghostty's own.

Swap in `nerd-appicon-light.icns` on a light desktop; the file is static, so it
does not follow the system appearance by itself.

Ghostty reads it when it reloads its config (`⌘⇧,`, the default keybind) or
starts again.

A real menu bar item would need its own app bundle and a different drawing —
macOS wants a monochrome template image there, no accent and no grid.

## Regenerating

```sh
node scripts/brand.mjs     # from the repository root
node scripts/dock-icns.mjs # the two .icns, from brand/icon-composer
```

The first redraws every app icon tile and re-renders every PNG; it never touches the
`.icns` or anything in `icon-composer`.
The lettering in it is an outline, so nothing needs Archivo installed; the
rasters go through headless Google Chrome.

The two logo files and the two wordmark SVGs are hand-made sources — the script
reads them, never writes them. To change the logo itself, edit the SVG.

The two rings of lettering in the logo are the one part that is drawn:

```sh
python3 scripts/logo-rings.py <Archivo-700.ttf> "WORLD CONTROL FOR YOUR TERMINAL" "SHELLS • AGENTS • YOU DECIDE"
node scripts/brand.mjs     # then the PNGs
```

It rewrites the lettering in both logo SVGs as outlines and leaves the globe and
the wordmark alone. The font is the static Archivo 700 from Google Fonts, read
from wherever it was downloaded to and never kept here. Letters are spread to
fill the arc, so a ring sits well at 26 to 36 characters; `•` separates the
parts, and a character the font lacks stops the script.
