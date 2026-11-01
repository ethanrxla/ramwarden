// The wardens' pixel pets: the corner pet that comments as you scroll, the
// big pet under the scroll world, and the color picker (docs/product-spec.md
// 2.5).
//
// Lines live in assets/pets/<warden>-lines.json so they can be edited without
// touching code. The comments are light: one joke every 45 to 90 seconds at
// most, 6 a visit, never while the visitor is typing, and one click mutes
// them for good. The site cannot see the visitor's machine, so a comment is
// only ever about the page (the section in view, the demo slider).

const store = {
  get(k) {
    try {
      return localStorage.getItem(k);
    } catch {
      return null;
    }
  },
  set(k, v) {
    try {
      if (v == null) localStorage.removeItem(k);
      else localStorage.setItem(k, v);
    } catch {
      // Private window or blocked storage: the setting lasts for this page.
    }
  },
};

// ---- Color --------------------------------------------------------------

// The body colors of each sprite, the ones a recolor moves. Outline, eyes
// and metal stay put, so the pet keeps its face whatever its color.
export const BODY_PALETTES = {
  ram: ["#2a1418", "#7a1822", "#a8232f", "#c4303d", "#f24455", "#f29ca6", "#ff7a86"],
  threat: ["#1a0f2e", "#3b0f7a", "#5b21b6", "#7c3aed", "#8b5cf6", "#c4b5fd", "#ddd6fe"],
};
export const PRESETS = [
  { name: "Crimson", hue: 354 },
  { name: "Amber", hue: 30 },
  { name: "Gold", hue: 48 },
  { name: "Mint", hue: 145 },
  { name: "Sky", hue: 205 },
  { name: "Violet", hue: 265 },
];

function hexToHsl(hex) {
  const n = parseInt(hex.slice(1), 16);
  const r = ((n >> 16) & 255) / 255, g = ((n >> 8) & 255) / 255, b = (n & 255) / 255;
  const max = Math.max(r, g, b), min = Math.min(r, g, b);
  const l = (max + min) / 2;
  if (max === min) return [0, 0, l];
  const d = max - min;
  const s = l > 0.5 ? d / (2 - max - min) : d / (max + min);
  const h = max === r ? (g - b) / d + (g < b ? 6 : 0) : max === g ? (b - r) / d + 2 : (r - g) / d + 4;
  return [h * 60, s, l];
}

function hslToHex(h, s, l) {
  const k = (n) => (n + h / 30) % 12;
  const a = s * Math.min(l, 1 - l);
  const f = (n) => Math.round(255 * (l - a * Math.max(-1, Math.min(k(n) - 3, 9 - k(n), 1))));
  return "#" + [f(0), f(8), f(4)].map((v) => v.toString(16).padStart(2, "0")).join("");
}

// Moves every body color of `svg` to `hue`, keeping each one's saturation and
// lightness so the shading survives. Other colors are left alone.
export function recolorSvg(svg, palette, hue) {
  const body = new Set(palette.map((c) => c.toLowerCase()));
  return svg.replace(/fill="(#[0-9a-fA-F]{6})"/g, (m, c) => {
    if (!body.has(c.toLowerCase())) return m;
    const [, s, l] = hexToHsl(c);
    return `fill="${hslToHex(((hue % 360) + 360) % 360, s, l)}"`;
  });
}

// One warden's color: remembered per browser, recolors every sprite of that
// warden on the page, and tells listeners so a signed-in page can save it.
export class PetColor extends EventTarget {
  constructor(warden, src) {
    super();
    this.warden = warden;
    this.src = src;
    this.svg = null;
    this.hue = Number.parseFloat(store.get(`rw.pet.color.${warden}`));
    if (!Number.isFinite(this.hue)) this.hue = null;
  }

  sprites() {
    return document.querySelectorAll(`img[data-sprite="${this.warden}"]`);
  }

  async load() {
    try {
      this.svg = await (await fetch(this.src)).text();
    } catch {
      this.svg = null; // the original sprite stays
    }
    this.paint();
  }

  set(hue) {
    this.hue = hue;
    store.set(`rw.pet.color.${this.warden}`, hue == null ? null : String(Math.round(hue)));
    this.paint();
    this.dispatchEvent(new CustomEvent("change", { detail: { warden: this.warden, hue } }));
  }

  paint() {
    let url = this.src;
    if (this.svg && this.hue != null) {
      url = "data:image/svg+xml;charset=utf-8," + encodeURIComponent(recolorSvg(this.svg, BODY_PALETTES[this.warden], this.hue));
    }
    for (const img of this.sprites()) img.src = url;
  }
}

// ---- Lines --------------------------------------------------------------

export async function loadLines(warden) {
  try {
    const r = await fetch(`assets/pets/${warden}-lines.json`);
    return await r.json();
  } catch {
    return { sections: {}, jokes: [] };
  }
}

// ---- The corner pet -----------------------------------------------------

const MUTE_KEY = "rw.pet.muted";
const COUNT_KEY = "rw.pet.jokes";

export class WardenPet {
  constructor(el, lines, { minGap = 45_000, maxGap = 90_000, maxJokes = 6 } = {}) {
    this.el = el;
    this.say = el.querySelector("[data-say]");
    this.muteButton = el.querySelector("[data-mute]");
    this.lines = lines;
    this.opts = { minGap, maxGap, maxJokes };
    this.lastKey = 0;
    this.joking = false;
    this.section = null;
    el.warden = this; // lets the tests call joke() without waiting a minute
  }

  get muted() {
    return store.get(MUTE_KEY) === "1";
  }

  start() {
    this.muteButton?.addEventListener("click", () => this.setMuted(!this.muted));
    this.setMuted(this.muted);
    addEventListener("keydown", () => (this.lastKey = Date.now()), { passive: true });
    this.watchSections();
    this.watchDemo();
    this.schedule();
  }

  setMuted(m) {
    store.set(MUTE_KEY, m ? "1" : null);
    this.el.classList.toggle("muted", m);
    if (this.muteButton) {
      this.muteButton.setAttribute("aria-pressed", String(m));
      this.muteButton.setAttribute("aria-label", m ? "Let the pet talk" : "Mute the pet");
      this.muteButton.textContent = m ? "Unmute" : "Mute";
    }
  }

  speak(line) {
    if (!line || this.muted) return;
    if (this.say.textContent === line) return;
    this.say.textContent = line;
    this.el.classList.remove("hop");
    void this.el.offsetWidth; // restart the hop animation
    this.el.classList.add("hop");
  }

  // The section most in view gets its line.
  watchSections() {
    const ids = Object.keys(this.lines.sections || {});
    const seen = new Map();
    const io = new IntersectionObserver(
      (entries) => {
        for (const e of entries) seen.set(e.target, e.intersectionRatio);
        let best = null;
        for (const [t, r] of seen) if (!best || r > seen.get(best)) best = t;
        if (!best || seen.get(best) <= 0 || this.joking) return;
        this.section = best.id;
        this.speak(this.lines.sections[best.id]);
      },
      { threshold: [0, 0.25, 0.5, 0.75, 1] },
    );
    for (const id of ids) {
      const s = document.getElementById(id);
      if (s) io.observe(s);
    }
  }

  // A remark when the demo slider crosses into high or low pressure.
  watchDemo() {
    const slider = document.querySelector("[data-pressure]");
    const demo = this.lines.demo;
    if (!slider || !demo) return;
    let band = "low";
    slider.addEventListener("input", () => {
      const next = Number(slider.value) >= 12 ? "high" : "low";
      if (next !== band) {
        band = next;
        this.speak(demo[next]);
      }
    });
  }

  typing() {
    const a = document.activeElement;
    return Date.now() - this.lastKey < 4000 || (a && (a.matches?.("input, textarea, select") || a.isContentEditable));
  }

  schedule() {
    const { minGap, maxGap } = this.opts;
    setTimeout(() => {
      this.joke();
      this.schedule();
    }, minGap + Math.random() * (maxGap - minGap));
  }

  // One joke, if the visit's allowance and the visitor allow it. Returns
  // whether it spoke.
  joke() {
    const jokes = this.lines.jokes || [];
    let count = 0;
    try {
      count = Number(sessionStorage.getItem(COUNT_KEY) || 0);
    } catch {
      // no session storage: count from zero
    }
    if (this.muted || this.typing() || !jokes.length || count >= this.opts.maxJokes) return false;
    try {
      sessionStorage.setItem(COUNT_KEY, String(count + 1));
    } catch {
      // ignore
    }
    this.joking = true;
    this.speak(jokes[Math.floor(Math.random() * jokes.length)]);
    setTimeout(() => {
      this.joking = false;
      if (this.section) this.speak(this.lines.sections[this.section]);
    }, 9000);
    return true;
  }
}

// ---- The big pet under the scroll world ---------------------------------

export class PetStage {
  constructor(el, lines, color) {
    this.el = el;
    this.lines = lines;
    this.color = color;
  }

  start() {
    this.talk();
    this.picker();
  }

  // Cycles through the stage lines while the section is on screen.
  talk() {
    const bubble = this.el.querySelector("[data-stage-say]");
    const lines = this.lines.stage || [];
    if (!bubble || !lines.length) return;
    let i = 0;
    let timer = null;
    const next = () => {
      bubble.textContent = lines[i++ % lines.length];
    };
    next();
    if (matchMedia("(prefers-reduced-motion: reduce)").matches) return;
    new IntersectionObserver(([e]) => {
      clearInterval(timer);
      if (e.isIntersecting) timer = setInterval(next, 4200);
    }).observe(this.el);
  }

  picker() {
    const box = this.el.querySelector("[data-colors]");
    if (!box) return;
    const slider = box.querySelector("input[type=range]");
    const mark = () => {
      for (const b of box.querySelectorAll("[data-hue]")) b.setAttribute("aria-pressed", String(b.dataset.hue === String(this.color.hue)));
      if (slider && this.color.hue != null) slider.value = String(Math.round(this.color.hue));
    };
    for (const p of PRESETS) {
      const b = document.createElement("button");
      b.type = "button";
      b.className = "swatch";
      b.dataset.hue = String(p.hue);
      b.title = p.name;
      b.setAttribute("aria-label", p.name);
      b.style.setProperty("--swatch", `hsl(${p.hue} 70% 55%)`);
      b.addEventListener("click", () => {
        this.color.set(p.hue);
        mark();
      });
      box.querySelector("[data-swatches]").append(b);
    }
    slider?.addEventListener("input", () => {
      this.color.set(Number(slider.value));
      mark();
    });
    box.querySelector("[data-reset]")?.addEventListener("click", () => {
      this.color.set(null);
      mark();
    });
    mark();
  }
}
