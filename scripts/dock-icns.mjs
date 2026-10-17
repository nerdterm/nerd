#!/usr/bin/env node
// Builds brand/nerd-appicon-{dark,light}.icns from the Icon Composer artwork in
// brand/icon-composer. Run from the repository root: node scripts/dock-icns.mjs
//
// The artwork is authored, not drawn here: it carries the shape, the shadow and the glass that
// macOS applies to an application bundle's icon and never applies to a dock tile fed a custom
// icon. ART is the share of the canvas checked in a real dock.

import { execFileSync, spawn } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const OUT = 'brand', SRC = 'brand/icon-composer', ART = 899 / 1024;
const CHROME = '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome';
const tmp = mkdtempSync(join(tmpdir(), 'nerd-icns-'));
const sleep = (ms) => new Promise(r => setTimeout(r, ms));

async function shot(page, out, w, h) {
  rmSync(out, { force: true });
  const p = spawn(CHROME, ['--headless', '--disable-gpu', `--screenshot=${out}`, `--window-size=${w},${h}`,
    '--hide-scrollbars', '--default-background-color=00000000', `--user-data-dir=${tmp}/profile`, page], { stdio: 'ignore' });
  for (let i = 0; i < 150 && !statSync(out, { throwIfNoEntry: false })?.size; i++) await sleep(200);
  await sleep(400);
  p.kill();
  if (!statSync(out, { throwIfNoEntry: false })?.size) throw new Error(`no screenshot: ${out} (is Chrome installed?)`);
}

for (const theme of ['dark', 'light']) {
  const a = Math.round(1024 * ART), pad = Math.round((1024 - a) / 2);
  const page = join(tmp, `${theme}.svg`);
  writeFileSync(page, `<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="1024">` +
    `<image x="${pad}" y="${pad}" width="${a}" height="${a}" href="data:image/png;base64,` +
    readFileSync(`${SRC}/nerd-icon-${theme}-1024.png`).toString('base64') + `"/></svg>`);
  const big = join(tmp, `${theme}-1024.png`);
  await shot(page, big, 1024, 1024);
  const set = join(tmp, `${theme}.iconset`);
  execFileSync('mkdir', ['-p', set]);
  for (const [px, file] of [[16, 'icon_16x16'], [32, 'icon_16x16@2x'], [32, 'icon_32x32'], [64, 'icon_32x32@2x'],
    [128, 'icon_128x128'], [256, 'icon_128x128@2x'], [256, 'icon_256x256'], [512, 'icon_256x256@2x'],
    [512, 'icon_512x512'], [1024, 'icon_512x512@2x']])
    execFileSync('sips', ['-z', `${px}`, `${px}`, big, '--out', join(set, `${file}.png`)], { stdio: 'ignore' });
  execFileSync('iconutil', ['-c', 'icns', set, '-o', `${OUT}/nerd-appicon-${theme}.icns`]);
}

rmSync(tmp, { recursive: true, force: true });
console.log(`${OUT}: nerd-appicon-{dark,light}.icns`);
