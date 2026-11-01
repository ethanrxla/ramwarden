// Scroll-scrubbed hero and the warden pet.
//
// The hero follows scroll-world (github.com/oso95/scroll-world, MIT): the
// page does not play the film, scroll drives its clock, so the ram clears the
// clutter exactly as fast as the visitor scrolls. Techniques taken from its
// scrub engine:
// - the clip is fetched once as a Blob, so it is always seekable, even from a
//   static host that ignores HTTP range requests;
// - seeks are coalesced: a new currentTime is never set while the decoder is
//   still seeking, so a fast flick cannot pile up and freeze the video;
// - the poster stays up until the first frame has actually painted;
// - phones get a smaller, tighter-keyframe encode.
// Written fresh rather than vendored because the engine injects a <style>
// element, which this site's CSP (style-src 'self') rightly refuses, and
// builds a whole page of its own; this is the part we need, in ~150 lines.

const reduceMotion = matchMedia("(prefers-reduced-motion: reduce)").matches;
const isPhone = () => matchMedia("(hover: none) and (pointer: coarse), (max-width: 860px)").matches;

export class ScrollScrub {
  constructor(section) {
    this.section = section;
    this.video = section.querySelector("video");
    this.beats = [...section.querySelectorAll("[data-from]")];
    this.bar = section.querySelector("[data-progress]");
    this.target = 0;
    this.ready = false;
  }

  async start() {
    if (reduceMotion) {
      // No scrubbing: show the closing frame and every line of copy.
      this.section.classList.add("still");
      this.beats.forEach((b) => b.classList.add("on"));
      return;
    }
    addEventListener("scroll", () => this.read(), { passive: true });
    addEventListener("resize", () => this.read());
    this.read();
    const src = (isPhone() && this.video.dataset.srcMobile) || this.video.dataset.src;
    try {
      const blob = await (await fetch(src)).blob();
      this.video.src = URL.createObjectURL(blob);
    } catch {
      this.video.src = src; // still plays; seeks may be coarser
    }
    this.video.addEventListener("loadeddata", () => {
      this.ready = true;
      this.section.classList.add("live");
      this.seek();
    });
    this.video.addEventListener("seeked", () => this.seek());
  }

  // Progress 0..1 through the section's scroll length.
  progress() {
    const r = this.section.getBoundingClientRect();
    const span = r.height - innerHeight;
    return span <= 0 ? 0 : Math.min(1, Math.max(0, -r.top / span));
  }

  read() {
    const p = this.progress();
    this.target = p;
    for (const b of this.beats) b.classList.toggle("on", p >= Number(b.dataset.from) && p < Number(b.dataset.to));
    if (this.bar) this.bar.style.transform = `scaleX(${p})`;
    this.seek();
  }

  seek() {
    const v = this.video;
    if (!this.ready || v.seeking || !v.duration) return;
    const t = this.target * (v.duration - 0.05);
    if (Math.abs(v.currentTime - t) > 1 / 30) v.currentTime = t;
  }
}

// A small pixel pet that rides along at the corner of the screen and says a
// line about whichever section is most in view (each section's
// data-pet-say). Dismissable, and it stays dismissed.
export class WardenPet {
  constructor(el) {
    this.el = el;
    this.say = el.querySelector("[data-say]");
    this.lines = [...document.querySelectorAll("[data-pet-say]")];
  }

  start() {
    if (localStorage.getItem("rw.pet.hidden") === "1") {
      this.el.hidden = true;
      return;
    }
    this.el.querySelector("[data-hide]")?.addEventListener("click", () => {
      this.el.hidden = true;
      localStorage.setItem("rw.pet.hidden", "1");
    });
    const seen = new Map();
    const io = new IntersectionObserver(
      (entries) => {
        for (const e of entries) seen.set(e.target, e.intersectionRatio);
        let best = null;
        for (const [t, r] of seen) if (!best || r > seen.get(best)) best = t;
        const line = best && seen.get(best) > 0 ? best.dataset.petSay : null;
        if (line && this.say.textContent !== line) {
          this.say.textContent = line;
          this.el.classList.remove("hop");
          void this.el.offsetWidth; // restart the hop animation
          this.el.classList.add("hop");
        }
      },
      { threshold: [0, 0.25, 0.5, 0.75, 1] },
    );
    this.lines.forEach((s) => io.observe(s));
  }
}

// The film with sound, in a dialog, for visitors who want to just watch it.
function setUpFilm() {
  const dialog = document.getElementById("film");
  const open = document.querySelector("[data-film]");
  if (!dialog || !open) return;
  const video = dialog.querySelector("video");
  open.addEventListener("click", () => {
    dialog.showModal();
    video.currentTime = 0;
    video.play().catch(() => {});
  });
  dialog.addEventListener("close", () => video.pause());
  dialog.querySelector("[data-close]")?.addEventListener("click", () => dialog.close());
}

if (typeof document !== "undefined") {
  const world = document.querySelector("[data-scrub]");
  if (world) new ScrollScrub(world).start();
  const pet = document.querySelector("[data-pet]");
  if (pet) new WardenPet(pet).start();
  setUpFilm();
}
