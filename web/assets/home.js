// Landing page: point the download buttons at the right files, highlight the
// visitor's own platform, and fill prices and limits from config.js so they
// are written down in one place.

import { config } from "./config.js";

const NAMES = { windows: "Windows", macos: "Mac", linux: "Linux" };

// Best guess at the visitor's OS. A wrong guess only changes which card is
// highlighted; every download stays one click away.
export function detectOs(nav = navigator) {
  const p = (nav.userAgentData?.platform || nav.platform || nav.userAgent || "").toLowerCase();
  if (p.includes("win")) return "windows";
  if (p.includes("mac") || p.includes("darwin")) return "macos";
  if (p.includes("linux") || p.includes("x11") || p.includes("cros")) return "linux";
  return null;
}

// The install block: one tab per OS, the visitor's own picked by default.
// A wrong guess costs one click.
function setUpInstall() {
  const box = document.querySelector("[data-install]");
  if (!box) return;
  const tabs = [...box.querySelectorAll("[data-os-tab]")];
  const pick = (os) => {
    for (const t of tabs) t.setAttribute("aria-selected", String(t.dataset.osTab === os));
    for (const p of box.querySelectorAll("[data-os-panel]")) p.hidden = p.dataset.osPanel !== os;
  };
  tabs.forEach((t) => t.addEventListener("click", () => pick(t.dataset.osTab)));
  pick(detectOs() || "macos");
  for (const b of box.querySelectorAll("[data-copy]")) {
    b.addEventListener("click", async () => {
      const cmd = b.parentElement.querySelector(".cmd").textContent;
      try {
        await navigator.clipboard.writeText(cmd);
        b.textContent = "Copied";
      } catch {
        b.textContent = "Select and copy";
      }
      setTimeout(() => (b.textContent = "Copy"), 1600);
    });
  }
  const hero = document.getElementById("hero-download");
  const os = detectOs();
  if (hero && os) hero.textContent = `Install for ${NAMES[os]}`;
}

function setUpDownloads() {
  for (const a of document.querySelectorAll('[data-download="releases"]')) a.href = config.releases;
}

// The italic catchline cycles through its lines, one every few seconds.
// Respects reduced motion by showing the first line only.
export function setUpRotators(root = document) {
  if (matchMedia("(prefers-reduced-motion: reduce)").matches) return;
  for (const el of root.querySelectorAll("[data-rotate]")) {
    let lines;
    try {
      lines = JSON.parse(el.dataset.rotate);
    } catch {
      continue;
    }
    const em = el.querySelector("em") || el;
    let i = Math.max(0, lines.indexOf(em.textContent));
    setInterval(() => {
      em.classList.add("out");
      setTimeout(() => {
        i = (i + 1) % lines.length;
        em.textContent = lines[i];
        em.classList.remove("out");
      }, 350);
    }, 3200);
  }
}

function setUpExtension() {
  const p = document.getElementById("extension-action");
  if (!p) return;
  if (config.chromeStore) {
    const a = document.createElement("a");
    a.className = "button";
    a.href = config.chromeStore;
    a.target = "_blank";
    a.rel = "noopener";
    a.textContent = "Add to Chrome";
    p.replaceChildren(a);
    return;
  }
  // Until the store listing is approved: load it unpacked from the source.
  p.replaceChildren(
    "Coming to the Chrome Web Store. Until then, open chrome://extensions, turn on Developer mode, click Load unpacked and choose the ",
    Object.assign(document.createElement("code"), { textContent: "extension" }),
    " folder from the ",
    Object.assign(document.createElement("a"), { href: config.repo, textContent: "source" }),
    ". It works in Brave and Edge too.",
  );
}

function fillPrices() {
  for (const el of document.querySelectorAll("[data-plan]")) {
    const plan = config.plans.find((p) => p.id === el.dataset.plan);
    if (plan) el.textContent = String(plan.usd);
  }
  for (const el of document.querySelectorAll("[data-limit]")) {
    const [tier, key] = el.dataset.limit.split(".");
    const v = config.limits[tier]?.[key];
    if (v != null) el.textContent = String(v);
  }
}

if (typeof document !== "undefined") {
  setUpInstall();
  setUpRotators();
  setUpDownloads();
  setUpExtension();
  fillPrices();
}
