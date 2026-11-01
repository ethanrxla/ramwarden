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

function setUpDownloads() {
  for (const a of document.querySelectorAll("[data-download]")) {
    const os = a.dataset.download;
    a.href = config.downloads[os] || config.releases;
    if (!config.downloads[os]) a.textContent = `Get it for ${NAMES[os]} on GitHub`;
  }
  const os = detectOs();
  const card = os && document.querySelector(`.download[data-os="${os}"]`);
  if (card) {
    card.classList.add("detected");
    card.querySelector(".tag").textContent = "YOUR COMPUTER";
    const hero = document.getElementById("hero-download");
    hero.textContent = `Download for ${NAMES[os]}`;
  }
}

function setUpExtension() {
  const p = document.getElementById("extension-action");
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
  setUpDownloads();
  setUpExtension();
  fillPrices();
}
