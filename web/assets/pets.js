// The wardens' pixel pets: the corner pet that comments as you scroll, the
// big pet under the scroll world, and the look controls (docs/product-spec.md
// 2.5).
//
// Lines live in assets/pets/<warden>-lines.json so they can be edited without
// touching code. The comments are light: one joke every 45 to 90 seconds at
// most, 6 a visit, never while the visitor is typing, and one click mutes
// them for good. The site cannot see the visitor's machine, so a comment is
// only ever about the page (the section in view, the demo slider).

import { DEFAULT_LOOKS, PUPILS, normalizeLook, spriteSvg, tint } from "./sprite.js";

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

// ---- Look ---------------------------------------------------------------

// The same controls as the Mac app's Colors pane: wool, horns, hooves and
// eyes, the pupil style, and (ThreatWarden) the visor. The sprite is redrawn
// from its pixel grid (sprite.js), so the warden keeps its own shape and
// only its colors change.
export const PRESETS = [
  { name: "Crimson", wool: "#f24455" },
  { name: "Amber", wool: "#f2913a" },
  { name: "Gold", wool: "#e8c547" },
  { name: "Mint", wool: "#3fcf86" },
  { name: "Sky", wool: "#3fa2f2" },
  { name: "Violet", wool: "#8b5cf6" },
];
export const PARTS = {
  ram: [["wool", "Wool"], ["horns", "Horns"], ["hooves", "Hooves"], ["eyes", "Eyes"]],
  threat: [["wool", "Wool"], ["horns", "Horns"], ["hooves", "Hooves"], ["eyes", "Eye"], ["visor", "Visor"]],
};

// A preset recolors the wool and gives the horns a darker shade of it, as the
// original ram has.
export function presetLook(warden, look, wool) {
  return normalizeLook(warden, { ...look, wool, horns: tint(wool, 0.8) });
}

// One warden's look: remembered per browser, redraws every sprite of that
// warden on the page, and tells listeners so a signed-in page can save it.
export class PetLook extends EventTarget {
  constructor(warden) {
    super();
    this.warden = warden;
    let saved = null;
    try {
      saved = JSON.parse(store.get(`rw.pet.look.${warden}`) || "null");
    } catch {
      saved = null;
    }
    this.custom = !!saved;
    this.look = normalizeLook(warden, saved || {});
  }

  sprites() {
    return document.querySelectorAll(`img[data-sprite="${this.warden}"]`);
  }

  // A look from the account, used when this browser has none of its own.
  adopt(look) {
    if (this.custom || !look) return;
    this.look = normalizeLook(this.warden, look);
    this.custom = true;
  }

  set(look) {
    this.look = normalizeLook(this.warden, look);
    this.custom = true;
    store.set(`rw.pet.look.${this.warden}`, JSON.stringify(this.look));
    this.paint();
    this.dispatchEvent(new CustomEvent("change", { detail: { warden: this.warden, look: this.look } }));
  }

  reset() {
    this.set(DEFAULT_LOOKS[this.warden]);
    this.custom = false;
    store.set(`rw.pet.look.${this.warden}`, null);
  }

  paint() {
    if (!this.custom) return; // the shipped SVG is the original look
    const url = "data:image/svg+xml;charset=utf-8," + encodeURIComponent(spriteSvg(this.warden, this.look));
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

  // The controls, built here so both tabs share them: quick wool colors,
  // one color well per part, the pupil style, Reset eyes and Original.
  picker() {
    const box = this.el.querySelector("[data-colors]");
    if (!box) return;
    const pet = this.color;
    const w = pet.warden;
    const el = (tag, attrs = {}, ...kids) => {
      const n = document.createElement(tag);
      for (const [k, v] of Object.entries(attrs)) n.setAttribute(k, v);
      n.append(...kids);
      return n;
    };
    const swatches = el("div", { class: "swatches", "data-swatches": "" });
    for (const p of PRESETS) {
      const b = el("button", { type: "button", class: "swatch", "data-wool": p.wool, title: p.name, "aria-label": `${p.name} wool` });
      b.style.setProperty("--swatch", p.wool);
      b.addEventListener("click", () => pet.set(presetLook(w, pet.look, p.wool)));
      swatches.append(b);
    }
    const parts = el("div", { class: "parts" });
    for (const [key, name] of PARTS[w]) {
      const input = el("input", { type: "color", "data-part": key, "aria-label": `${name} color` });
      input.addEventListener("input", () => pet.set({ ...pet.look, [key]: input.value }));
      parts.append(el("label", { class: "part" }, input, name));
    }
    const select = el("select", { "data-part": "pupil", "aria-label": "Pupils" });
    for (const p of PUPILS) select.append(el("option", { value: p.id }, p.name));
    select.addEventListener("change", () => pet.set({ ...pet.look, pupil: select.value }));
    parts.append(el("label", { class: "part pupils" }, "Pupils", select));
    const eyes = el("button", { type: "button", class: "button secondary small", "data-reset-eyes": "" }, "Reset eyes");
    eyes.addEventListener("click", () => {
      const d = DEFAULT_LOOKS[w];
      pet.set({ ...pet.look, eyes: d.eyes, pupil: d.pupil });
    });
    const all = el("button", { type: "button", class: "button secondary small", "data-reset": "" }, "Original");
    all.addEventListener("click", () => pet.reset());
    box.replaceChildren(swatches, parts, el("div", { class: "row-actions" }, eyes, all));

    const mark = () => {
      for (const b of box.querySelectorAll("[data-wool]")) b.setAttribute("aria-pressed", String(b.dataset.wool === pet.look.wool));
      for (const i of box.querySelectorAll("[data-part]")) i.value = pet.look[i.dataset.part];
    };
    pet.addEventListener("change", mark);
    mark();
  }
}
