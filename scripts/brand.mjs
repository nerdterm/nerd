#!/usr/bin/env node
// Draws the app icon tiles and rasterises everything in brand/ that is derived.
// Run from the repository root: node scripts/brand.mjs (--wordmark: only the sidebar's wordmark rasters)
//
// Hand-drawn sources it never touches: nerd-logo-{light,dark}.svg and
// nerd-wordmark-{light,dark}.svg, whose lettering is already outlines.
// The N below is an outline too, so no asset depends on a font being installed.

import { execFileSync, spawn } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const OUT = 'brand';
const N = 'M54.7 141.3L66.2 58.7L86.7 58.7L114.2 95.1Q114.6 95.7 115.5 96.9Q116.3 98.1 117.4 99.4Q118.4 100.7 119.1 101.8L120.1 101.8Q120.3 100.1 120.6 97.3Q121.0 94.5 121.4 91.8L126.0 58.7L148.1 58.7L136.6 141.3L116.3 141.3L90.0 106.5Q88.4 104.3 86.6 101.9Q84.7 99.4 83.7 97.8L82.8 97.8Q82.6 99.5 82.2 102.6Q81.7 105.6 81.3 108.8L76.8 141.3L54.7 141.3Z';
const N_SIZE = 120, N_WIDTH = 96; // what that outline was exported at

// One drawing per size band: below 40 px the globe grows into the tile and the grid thins,
// at 16 px only the rim is left. Names carry the band, not the pixels.
// body is Apple's grid, 824 of a 1024 canvas, the rest margin the shadow spends; the small bands
// round it to a whole pixel at their own size so the rim stays crisp. Apple's own slices measure
// 206 of 256, 24 of 32 and 12 of 16 solid, reaching 216, 26 and 14 once their shadow is counted.
const TIERS = {
  '': {body: 824 / 1024, sh: 1, rx: 84, ry: 52, merid: 4, paral: 3, trim: 1, sw: 4.5, outline: 7, n: 120, nw: 96, cut: 14},
  '-32px': {body: 24 / 32, sh: 2, rx: 91, ry: 58, merid: 2, paral: 1, trim: 0, sw: 8, outline: 12, n: 134, nw: 108, cut: 20},
  '-16px': {body: 12 / 16, sh: 4, rx: 95, ry: 62, merid: 1, paral: 0, trim: 0, sw: 9, outline: 15, n: 148, nw: 120, cut: 26},
};

const THEMES = {
  light: {ground: '#F3F3EF', ink: '#141414', letter: '#141414'},
  dark: {ground: '#16171D', ink: '#9FD08A', letter: '#D6D8E2'},
};

// One corner of the shape macOS masks every app icon with, fitted to that mask to 0.38 px rms on
// an 824 body: two cubics from the edge at l to the diagonal at m, as fractions of the side.
const SQ = {l: 0.34031, m: 0.07641, h: 0.16281, d: 0.07312 / Math.SQRT2};

// The decoration Apple bakes into a shipped icon file, in tile units so it scales with the band:
// a downward drop shadow outside the body and a light rim inside it. Nothing draws these for us —
// the Dock decorates an app bundle's icon, not a raw image handed to NSApplication.icon.
const SHADOW = {dy: 1.94, sd: 4.37, op: 0.22};
const RIM = {w: 5.5, sd: 1.2, op: 0.13};

// That corner four times, a quarter turn apart.
function squircle(s) {
  const f = (v) => (v * s).toFixed(2);
  let p = [[SQ.l, 0], [SQ.l - SQ.h, 0], [SQ.m + SQ.d, SQ.m - SQ.d], [SQ.m, SQ.m],
           [SQ.m - SQ.d, SQ.m + SQ.d], [0, SQ.l - SQ.h], [0, SQ.l]];
  let d = `M${f(1 - SQ.l)} 0`;
  for (let k = 0; k < 4; k++) {
    const c = p.map(([x, y]) => `${f(x)} ${f(y)}`);
    d += `L${c[0]}C${c[1]} ${c[2]} ${c[3]}C${c[4]} ${c[5]} ${c[6]}`;
    p = p.map(([x, y]) => [y, 1 - x]);
  }
  return d + 'Z';
}

// Meridians are ellipses rx*sin(lon), parallels are chords at ry*sin(lat). trim drops the line
// nearest the rim on each axis: at equal longitudes they bunch there and the edge reads as noise.
function globe({rx, ry, merid, paral, trim, sw, outline, color}) {
  const cx = 100, cy = 100;
  let s = `<g fill="none" stroke="${color}" stroke-width="${sw}" stroke-linecap="round">`;
  for (let i = 0; i < merid - trim; i++) {
    const r = rx * Math.sin(i * Math.PI / 2 / merid);
    s += i === 0 ? `<line x1="${cx}" y1="${cy - ry}" x2="${cx}" y2="${cy + ry}"/>`
                 : `<ellipse cx="${cx}" cy="${cy}" rx="${r.toFixed(1)}" ry="${ry}"/>`;
  }
  for (let j = -paral + 1 + trim; j < paral - trim; j++) {
    const y = ry * Math.sin(j * Math.PI / 2 / paral);
    const hw = rx * Math.sqrt(1 - (y / ry) ** 2);
    s += `<line x1="${(cx - hw).toFixed(1)}" y1="${(cy + y).toFixed(1)}" x2="${(cx + hw).toFixed(1)}" y2="${(cy + y).toFixed(1)}"/>`;
  }
  return s + `<ellipse cx="${cx}" cy="${cy}" rx="${rx}" ry="${ry}" stroke-width="${outline}"/></g>`;
}

// The mask cuts the grid away around the letter, so grid and N never touch.
function tile(t, th, at = 200) {
  const pad = 100 * (1 / t.body - 1), view = 200 + 2 * pad;
  const fx = t.nw / N_WIDTH, fy = t.n / N_SIZE;
  const letter = (paint) => `<g transform="translate(100 100) scale(${fx.toFixed(4)} ${fy.toFixed(4)}) translate(-100 -100)">` +
    `<path d="${N}" ${paint}/></g>`;
  const body = squircle(200);
  return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="${(-pad).toFixed(2)} ${(-pad).toFixed(2)} ${view.toFixed(2)} ${view.toFixed(2)}" width="${at}" height="${at}" role="img" aria-label="nerd">` +
    `<defs><filter id="drop" x="-20%" y="-20%" width="140%" height="140%">` +
    `<feDropShadow dx="0" dy="${SHADOW.dy}" stdDeviation="${SHADOW.sd * t.sh}" flood-opacity="${SHADOW.op}"/></filter>` +
    `<filter id="soft"><feGaussianBlur stdDeviation="${RIM.sd}"/></filter>` +
    `<clipPath id="body"><path d="${body}"/></clipPath></defs>` +
    `<path d="${body}" fill="${th.ground}" filter="url(#drop)"/>` +
    `<mask id="cut"><rect width="200" height="200" fill="#fff"/>` +
    letter(`fill="#000" stroke="#000" stroke-width="${(t.cut / fy).toFixed(1)}" stroke-linejoin="round"`) + `</mask>` +
    `<g mask="url(#cut)">${globe({...t, color: th.ink})}</g>` +
    letter(`fill="${th.letter}"`) +
    `<g clip-path="url(#body)"><path d="${body}" fill="none" stroke="#fff" stroke-opacity="${RIM.op}"` +
    ` stroke-width="${RIM.w}" filter="url(#soft)"/></g>` + `</svg>`;
}

const CHROME = '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome';
const tmp = mkdtempSync(join(tmpdir(), 'nerd-brand-'));
const sleep = (ms) => new Promise(r => setTimeout(r, ms));
let shots = 0;

// Chrome writes the screenshot and then keeps running, so it is killed once the file has landed.
// A profile of its own each time: a dying Chrome still holds its profile's lock, and the next one would write nothing.
// w and h are the page's own size; scale multiplies the pixels, as an svg is drawn at the size it declares.
async function shot(page, out, w, h, scale = 1) {
  rmSync(out, {force: true});
  const proc = spawn(CHROME, ['--headless', '--disable-gpu', `--screenshot=${out}`,
    `--window-size=${w},${h}`, `--force-device-scale-factor=${scale}`, '--hide-scrollbars', '--default-background-color=00000000',
    `--user-data-dir=${tmp}/profile${shots++}`, page], {stdio: 'ignore'});
  for (let i = 0; i < 150 && !statSync(out, {throwIfNoEntry: false})?.size; i++) await sleep(200);
  await sleep(400);
  proc.kill();
  if (!statSync(out, {throwIfNoEntry: false})?.size) throw new Error(`no screenshot: ${out} (is Chrome installed?)`);
}

// A tile rendered at a chosen pixel size: the svg carries that size, Chrome screenshots it.
async function png(t, th, px, out) {
  const src = join(tmp, `t${px}.svg`);
  writeFileSync(src, tile(t, th, px));
  await shot(src, out, px, px);
}

const wordmarkOnly = process.argv.includes('--wordmark');

if (!wordmarkOnly)
  for (const [band, t] of Object.entries(TIERS))
    for (const [name, th] of Object.entries(THEMES))
      writeFileSync(`${OUT}/nerd-appicon-${name}${band}.svg`, tile(t, th) + '\n');

// A raster per size anything asks for, each drawn from the band that size belongs to rather
// than downscaled from the one above it, which is the whole reason three bands exist.
const RASTERS = [16, 32, 64, 128, 256, 512, 1024];
const bandFor = (px) => px <= 16 ? '-16px' : px < 64 ? '-32px' : '';

// The sidebar draws the wordmark nine cells by one row: 4.5:1 is that box at a 1:2 cell, so it is not stretched.
// One raster per row height in device pixels, the list in src/ui.rs: terminals shrink images without
// mipmaps, so each is drawn near the size it is shown at.
const ROWS = [16, 20, 24, 28, 32, 36, 40, 48, 64];

for (const [name, th] of Object.entries(THEMES)) {
  if (!wordmarkOnly) {
    for (const px of RASTERS) await png(TIERS[bandFor(px)], th, px, `${OUT}/nerd-appicon-${name}-${px}.png`);
    // Both lockups at twice their canvas: enough for retina, and nothing here is ever printed.
    await shot(`${OUT}/nerd-logo-${name}.svg`, `${OUT}/nerd-logo-${name}-1792x808.png`, 896, 404, 2);
  }
  const svg = readFileSync(`${OUT}/nerd-wordmark-${name}.svg`, 'utf8');
  for (const h of ROWS) {
    const src = join(tmp, `wordmark-${name}-${h}.svg`);
    writeFileSync(src, svg.replace(/width="\d+" height="\d+"/, `width="${4.5 * h}" height="${h}"`));
    await shot(src, `${OUT}/nerd-wordmark-${name}-${h}.png`, 4.5 * h, h);
  }
}

// The two .icns are not generated here. They are built from the Icon Composer artwork in
// brand/icon-composer by scripts/dock-icns.mjs, and that artwork is the approved icon.

// The last Chrome is still writing its profile as it dies, so the sweep retries.
rmSync(tmp, {recursive: true, force: true, maxRetries: 20, retryDelay: 200});
console.log(`${OUT}: ${wordmarkOnly ? '' : 'app icon svg+png, '}wordmark png`);
