// The wardens' 8-bit sprites, drawn in the browser so the visitor can dress
// them up the way the Mac app does (macos/menubar Companion.swift, ColorsView).
//
// The grid is the app's PixelRam, ported cell for cell (as in
// scripts/warden-sprites.py, which wrote the shipped ram.svg and threat.svg).
// Each cell holds a palette key, and the visitor's colors fill the keys:
// wool (w, shade s, highlight b), horns (h, H, j), hooves (k) and eyes (e, E),
// with the same tint factors as the app. ThreatWarden is the same ram with
// its left half turned to machine, plus a visor, an antenna and a chest core;
// its metal never changes color.
//
// RamWarden stays the plain ram: no antenna, no plates. Only colors change.

const N = 48;

// Each warden's original look. "Original" on the page goes back to this.
export const DEFAULT_LOOKS = {
  ram: { wool: "#f24455", horns: "#c4303d", hooves: "#2a1418", eyes: "#ffffff", pupil: "slit" },
  threat: { wool: "#8b5cf6", horns: "#7c3aed", hooves: "#1a0f2e", eyes: "#39ff88", pupil: "slit", visor: "#39ff88" },
};

// The hand-picked shades of the shipped sprites, used while a part keeps its
// original color, so the default sprite matches the shipped SVG exactly.
const ORIGINAL_SHADES = {
  ram: { o: "#0d0b0e", s: "#a8232f", b: "#ff7a86", H: "#f29ca6", j: "#7a1822", E: "#cfd6dd" },
  threat: { o: "#07050b", s: "#5b21b6", b: "#c4b5fd", H: "#ddd6fe", j: "#3b0f7a", E: "#a7f3c7", V: "#d9ffe9", R: "#eafff2" },
};
const METAL = { m: "#9aa3ad", M: "#e3e8ee", d: "#5b636d" };

export const PUPILS = [
  { id: "slit", name: "Ram slits" },
  { id: "round", name: "Round" },
  { id: "none", name: "Glowing / none" },
];

const HEX = /^#[0-9a-f]{6}$/i;

// Fills in anything missing or malformed from the warden's default.
export function normalizeLook(warden, look = {}) {
  const base = DEFAULT_LOOKS[warden];
  const out = { ...base };
  for (const k of Object.keys(base)) {
    const v = look?.[k];
    if (k === "pupil") {
      if (PUPILS.some((p) => p.id === v)) out.pupil = v;
    } else if (typeof v === "string" && HEX.test(v)) out[k] = v.toLowerCase();
  }
  return out;
}

// The app's tint: each channel times `factor`, capped at full.
export function tint(hex, factor) {
  const n = parseInt(hex.slice(1), 16);
  const ch = (v) => Math.min(255, Math.round(v * factor)).toString(16).padStart(2, "0");
  return "#" + ch((n >> 16) & 255) + ch((n >> 8) & 255) + ch(n & 255);
}

export function palette(warden, look) {
  const d = DEFAULT_LOOKS[warden];
  const o = ORIGINAL_SHADES[warden];
  const keep = (part) => look[part] === d[part];
  const p = {
    o: o.o,
    w: look.wool,
    s: keep("wool") ? o.s : tint(look.wool, 0.62),
    b: keep("wool") ? o.b : tint(look.wool, 1.3),
    h: look.horns,
    H: keep("horns") ? o.H : tint(look.horns, 1.25),
    j: keep("horns") ? o.j : tint(look.horns, 0.66),
    k: look.hooves,
    e: look.eyes,
    E: keep("eyes") ? o.E : tint(look.eyes, 0.76),
  };
  if (warden === "threat") {
    Object.assign(p, METAL);
    p.v = look.visor;
    p.V = keep("visor") ? o.V : tint(look.visor, 1.6);
    p.r = look.visor; // the right eye's glint, the antenna tip and the core glow with the visor
    p.R = keep("visor") ? o.R : tint(look.visor, 1.8);
  }
  return p;
}

class Grid {
  constructor() {
    this.c = new Array(N * N).fill(".");
  }
  put(x, y, k) {
    if (x >= 0 && x < N && y >= 0 && y < N) this.c[y * N + x] = k;
  }
  get(x, y) {
    return this.c[y * N + x];
  }
  box(x, y, w, h, k) {
    for (let yy = y; yy < y + h; yy++) for (let xx = x; xx < x + w; xx++) this.put(xx, yy, k);
  }
  ellipse(cx, cy, rx, ry, wool) {
    for (let y = 0; y < N; y++) {
      for (let x = 0; x < N; x++) {
        const dx = x - cx, dy = y - cy;
        if ((dx * dx) / (rx * rx) + (dy * dy) / (ry * ry) > 1) continue;
        const edge = (dx * dx) / (rx - 1.2) ** 2 + (dy * dy) / (ry - 1.2) ** 2 > 1;
        let k = "k";
        if (wool) k = edge ? "o" : dx > rx * 0.45 || dy > ry * 0.55 ? "s" : dx < -rx * 0.35 && dy < 0 ? "b" : "w";
        this.put(x, y, k);
      }
    }
  }
}

// The app's pupil styles, on an eye whose white starts at column x.
function pupil(g, x, style, glint) {
  if (style === "slit") {
    g.box(x + 1, 21, 5, 1, "o");
    if (glint) g.box(x + 2, 21, 2, 1, glint);
  } else if (style === "round") {
    g.box(x + 3, 21, 2, 2, "o");
    g.put(x + 3, 20, "e");
  }
}

function ram(g, look) {
  g.box(17, 42, 5, 6, "o"); g.box(27, 42, 5, 6, "o"); g.box(18, 43, 3, 4, "k"); g.box(28, 43, 3, 4, "k");
  g.ellipse(24, 38, 9, 7, true);
  g.box(18, 34, 2, 3, "b"); g.box(29, 35, 2, 3, "s");
  for (const center of [8, 40]) {
    for (let y = 10; y < 33; y++) {
      for (let x = 0; x < N; x++) {
        const dx = x - center, dy = y - 21;
        const r = Math.hypot(dx, dy);
        if (!(r >= 3.7 && r <= 9.4)) continue;
        const inward = center < 24 ? dx > 1 : dx < -1;
        if (inward && dy > 3) continue;
        g.put(x, y, r > 8.2 || r < 4.8 ? "o" : dy < -2 ? "H" : dy > 3 ? "j" : "h");
      }
    }
    g.box(center - 1, 20, 3, 2, "h"); g.box(center, 21, 2, 4, "H");
  }
  g.ellipse(11, 23, 5, 4, true); g.ellipse(37, 23, 5, 4, true);
  for (let y = 12; y < 37; y++) {
    const half = y < 17 ? 8 + (y - 12) : y < 23 ? 12 : Math.max(4, 12 - Math.floor(((y - 22) * 2) / 3));
    for (let x = 24 - half; x <= 24 + half; x++) {
      const edge = x === 24 - half || x === 24 + half || y === 12 || y === 36;
      g.put(x, y, edge ? "o" : x > 27 || y > 29 ? "s" : x < 19 ? "b" : "w");
    }
  }
  g.box(21, 10, 3, 2, "o"); g.box(24, 9, 2, 3, "o"); g.box(22, 11, 5, 3, "b");
  for (const x of [15, 26]) {
    g.box(x - 1, 19, 9, 6, "o"); g.box(x, 20, 7, 4, "e"); g.box(x + 1, 23, 5, 1, "E");
    pupil(g, x, look.pupil);
  }
  g.box(22, 27, 5, 3, "b"); g.box(20, 31, 9, 2, "o"); g.box(21, 33, 7, 1, "o"); g.put(21, 31, "s"); g.put(27, 31, "s");
  g.box(23, 35, 3, 1, "o"); g.box(22, 37, 5, 2, "o"); g.box(23, 37, 3, 3, "s"); g.put(24, 40, "o");
}

function cyborg(g, look) {
  const metal = { w: "m", b: "M", s: "d", h: "m", H: "M", j: "d", k: "d" };
  for (let y = 0; y < N; y++) for (let x = 0; x < 24; x++) {
    const k = g.get(x, y);
    if (metal[k]) g.put(x, y, metal[k]);
  }
  for (let y = 13; y < 36; y++) if (!".o".includes(g.get(24, y))) g.put(24, y, "o");
  for (const [x, y] of [[19, 15], [19, 28], [14, 32], [21, 34], [9, 26], [13, 24]]) {
    if (!".o".includes(g.get(x, y))) g.put(x, y, x === 9 && y === 26 ? "r" : "M");
  }
  for (let x = 14; x < 24; x++) if (!".o".includes(g.get(x, 26))) g.put(x, 26, "d");
  g.box(13, 19, 11, 6, "o"); g.box(14, 20, 9, 4, "v"); g.box(15, 21, 4, 1, "V"); g.box(20, 22, 2, 1, "V");
  g.box(26, 20, 7, 4, "e");
  pupil(g, 26, look.pupil, "r");
  g.box(27, 23, 5, 1, "E");
  for (let y = 3; y < 12; y++) g.put(17, y, "d");
  g.put(16, 4, "d"); g.put(18, 4, "d");
  g.box(16, 1, 3, 2, "r"); g.put(17, 0, "o"); g.put(15, 1, "o"); g.put(19, 1, "o"); g.put(15, 2, "o"); g.put(19, 2, "o");
  g.box(16, 3, 3, 1, "o"); g.put(17, 1, "R");
  g.box(18, 43, 3, 4, "d"); g.box(18, 44, 3, 1, "M");
  g.box(22, 39, 3, 3, "o"); g.put(23, 40, "v");
}

// The cells of a warden's sprite: an array of 48*48 palette keys.
export function cells(warden, look) {
  const g = new Grid();
  ram(g, look);
  if (warden === "threat") cyborg(g, look);
  return g.c;
}

const NAMES = { ram: "RamWarden", threat: "ThreatWarden" };

// The sprite as an SVG document, one rect per run of same-colored cells.
export function spriteSvg(warden, look) {
  look = normalizeLook(warden, look);
  const c = cells(warden, look);
  const pal = palette(warden, look);
  const rects = [];
  for (let y = 0; y < N; y++) {
    let x = 0;
    while (x < N) {
      const k = c[y * N + x];
      if (k === ".") { x++; continue; }
      let x2 = x;
      while (x2 < N && c[y * N + x2] === k) x2++;
      rects.push(`<rect x="${x}" y="${y}" width="${x2 - x}" height="1" fill="${pal[k]}"/>`);
      x = x2;
    }
  }
  return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 ${N} ${N}" width="96" height="96" shape-rendering="crispEdges" role="img" aria-label="${NAMES[warden]}"><title>${NAMES[warden]}</title>${rects.join("")}</svg>`;
}
