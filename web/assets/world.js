// The scroll world: section 3 of both warden tabs.
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
// builds a whole page of its own; this is the part we need.
//
// ThreatWarden's world has no film: the same component drives a CSS scene
// through `--p` (progress 0..1) and `[data-at]` elements that switch on as
// the visitor scrolls past them.

const reduceMotion = matchMedia("(prefers-reduced-motion: reduce)").matches;
const isPhone = () => matchMedia("(hover: none) and (pointer: coarse), (max-width: 860px)").matches;

export class ScrollScrub {
  constructor(section) {
    this.section = section;
    this.video = section.querySelector("video");
    this.beats = [...section.querySelectorAll("[data-from]")];
    this.marks = [...section.querySelectorAll("[data-at]")];
    this.bar = section.querySelector("[data-progress]");
    this.target = 0;
    this.ready = false;
  }

  async start() {
    if (reduceMotion) {
      // No scrubbing: show the closing frame and every line of copy.
      this.section.classList.add("still");
      this.beats.forEach((b) => b.classList.add("on"));
      this.marks.forEach((m) => m.classList.add("on"));
      this.section.style.setProperty("--p", "1");
      return;
    }
    addEventListener("scroll", () => this.read(), { passive: true });
    addEventListener("resize", () => this.read());
    this.read();
    if (!this.video) {
      this.section.classList.add("live");
      return;
    }
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
    for (const m of this.marks) m.classList.toggle("on", p >= Number(m.dataset.at));
    this.section.style.setProperty("--p", p.toFixed(3));
    if (this.bar) this.bar.style.transform = `scaleX(${p})`;
    this.seek();
  }

  seek() {
    const v = this.video;
    if (!v || !this.ready || v.seeking || !v.duration) return;
    const t = this.target * (v.duration - 0.05);
    if (Math.abs(v.currentTime - t) > 1 / 30) v.currentTime = t;
  }
}

if (typeof document !== "undefined") {
  const world = document.querySelector("[data-scrub]");
  if (world) new ScrollScrub(world).start();
}
