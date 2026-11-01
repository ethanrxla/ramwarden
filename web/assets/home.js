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

const TIER = { included: "Included in every plan", addon: "Pro add-on", pro: "Pro" };

// One card per warden in config.js, then an empty slot that says more are
// coming, so a new warden is a config entry and an icon, not a page edit.
function renderWardens() {
  const list = document.getElementById("warden-list");
  if (!list) return;
  const el = (tag, cls, text) => Object.assign(document.createElement(tag), cls ? { className: cls } : {}, text != null ? { textContent: text } : {});
  const cards = config.wardens.map((w) => {
    const card = el("div", "warden card");
    const head = el("div", "warden-head");
    const icon = el("img");
    icon.src = w.icon;
    icon.alt = "";
    const title = el("div");
    title.append(el("h3", null, w.name), el("span", "job", w.job));
    head.append(icon, title);
    const badges = el("div", "badges");
    badges.append(el("span", `badge ${w.tier}`, TIER[w.tier] || w.tier));
    if (w.status === "soon") badges.append(el("span", "badge", "Coming soon"));
    card.append(head, el("p", null, w.summary), badges);
    return card;
  });
  const next = el("div", "warden card next");
  const head = el("div", "warden-head");
  const icon = el("img");
  icon.src = "assets/warden-next.svg";
  icon.alt = "";
  const title = el("div");
  title.append(el("h3", null, "More wardens"), el("span", "job", "On the way"));
  head.append(icon, title);
  next.append(head, el("p", null, "Each new warden takes one job off your plate and joins as an add-on, so you only pay for the ones you want."));
  list.replaceChildren(...cards, next);
}

if (typeof document !== "undefined") {
  renderWardens();
  setUpDownloads();
  setUpExtension();
  fillPrices();
}
