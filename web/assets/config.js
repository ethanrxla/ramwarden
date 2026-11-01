// Site settings. Everything a deploy might change lives here, so the pages
// themselves never need editing to move the API, change a price or publish a
// release.
//
// The prices below are the ones in docs/product-spec.md. The server is the
// authority: when `/v1/checkout/config` answers, its plans replace these on
// the account page, so a price change there cannot leave checkout selling the
// old one.

export const config = {
  // The hosted API (`ramwarden-api`). Changing it also means changing the
  // `connect-src` in each page's Content-Security-Policy.
  apiBase: "https://api.ramwarden.app",

  repo: "https://github.com/ethanrxla/ramwarden",
  releases: "https://github.com/ethanrxla/ramwarden/releases/latest",

  // Chrome Web Store listing. Null until the store review passes; until then
  // the page explains how to load the extension unpacked.
  chromeStore: null,

  // Three cards (docs/product-spec.md 1.2, as changed 2026-11-01):
  // - Lite (free): the fixed-rules engine with no AI, on one computer. A free
  //   account (email or wallet, no card) is what makes "1 computer" true:
  //   devices are counted per account.
  // - Plus (tier id "ram"): adds the AI warden. Upgrades are enforced by
  //   the signed entitlement, not by the page. The only plan with a trial.
  // - Pro: adds ThreatWarden. Paid from day one.
  // Plan ids are the checkout contract's. Bullets promise only what ships.
  trialDays: 14,
  lite: {
    name: "RamWarden Lite",
    tagline: "Keep it tidy.",
    cta: "Get it free",
    features: [
      "See what's really eating your memory, live",
      "Frees memory on simple, built-in rules",
      "Tab budget: old tabs close on their own",
      "Games, calls, terminals and VMs are never touched",
      "Your warden in its original colors",
      "1 computer, free account, no card",
    ],
  },
  tiers: {
    ram: {
      name: "RamWarden Plus",
      tagline: "Let it decide.",
      badge: "Most popular",
      trialDays: 14,
      devices: 5,
      features: [
        "Everything in Lite, plus:",
        "An AI warden that picks what you won't miss",
        "Learns your habits and keeps what you'll reopen",
        "Never closes a tab with unsaved work",
        "Pauses idle apps when memory gets tight, resumed the moment you return",
        "Explains every call in plain words",
        "Dress up your warden: wool, horns, hooves, eyes",
        "5 computers: Windows, macOS and Linux",
      ],
    },
    pro: {
      name: "RamWarden Pro",
      tagline: "Fast and safe.",
      trialDays: 0,
      devices: 5,
      features: [
        "Everything in Plus, plus:",
        "ThreatWarden watches this computer for threats",
        "One plain status: Clear, Watch or Alert",
        "Every alert explained in plain words",
        "Its own pane in your menu bar or tray",
        "The cyborg warden",
      ],
    },
  },
  plans: [
    { id: 1, tier: "ram", interval: "month", usd: 4 },
    { id: 2, tier: "pro", interval: "month", usd: 5 },
    { id: 3, tier: "ram", interval: "year", usd: 20 },
    { id: 4, tier: "pro", interval: "year", usd: 24 },
  ],
};

// Finds a plan by tier and interval.
export function planFor(plans, tier, interval) {
  return plans.find((p) => p.tier === tier && p.interval === interval) || null;
}

// What a year of the annual plan saves against twelve months, in whole percent.
export function annualSaving(plans, tier) {
  const m = planFor(plans, tier, "month");
  const y = planFor(plans, tier, "year");
  if (!m || !y) return 0;
  return Math.round((1 - y.usd / (m.usd * 12)) * 100);
}
