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

  // Three cards (docs/product-spec.md 1.2, as changed 2026-11-01/02):
  // - Lite (free): the memory algorithm with no AI and no warden pet, on one
  //   computer. A free account (email or wallet, no card) is what makes
  //   "1 computer" true: devices are counted per account.
  // - Plus (tier id "ram"): the AI and the warden pet, 3 computers. The only
  //   plan with a trial.
  // - Pro: adds ThreatWarden, 5 computers. Paid from day one.
  // Upgrades are enforced by the server and the signed entitlement, not by
  // the page. Every bullet is something the code does, with its number where
  // it has one (README, docs/specs/browser-memory.md, threatwarden.md).
  // `soon` marks a part that is specified but not shipped yet.
  trialDays: 14,
  lite: {
    name: "RamWarden Lite",
    tagline: "Keep it tidy.",
    cta: "Get it free",
    features: [
      "Memory measured the way your OS charges it: shared browser memory counted once, not per tab",
      "Acts on memory stalls (time lost waiting on RAM), not on a full meter",
      "A 4-step reclaim algorithm: compress cold memory, page it out, unload idle tabs, pause idle apps you list",
      "Every app ranked least recently used first",
      "Tab budget: your 6 most recent tabs stay, others close after 60 idle minutes, with a reopen list",
      "Every pause undone the moment memory recovers",
      "Games, anti-cheat, calls, terminals and VMs are never touched",
      "1 computer, free account, no card",
    ],
  },
  tiers: {
    ram: {
      name: "RamWarden Plus",
      tagline: "Let it decide.",
      badge: "Most popular",
      trialDays: 14,
      devices: 3,
      features: [
        "Everything in Lite, plus:",
        "A customizable AI warden pet that picks what you won't miss",
        "AI scores each idle app: your chance of coming back within the hour",
        "Pauses an app only when 70% sure you won't return, closes one only at 85%",
        "Closes tabs you won't return to, each saved to a reopen list",
        "A model trained daily on your own habits, used only once it beats plain least recently used",
        "3 computers: Windows, macOS and Linux",
      ],
    },
    pro: {
      name: "RamWarden Pro",
      tagline: "Fast and safe.",
      trialDays: 0,
      devices: 5,
      features: [
        "Everything in Plus, plus ThreatWarden:",
        "Sentinel: file integrity, log analysis, rootkit and vulnerable software checks",
        { text: "Antivirus: malware sweeps when memory is free, plus scan on demand", soon: true },
        { text: "Network monitor: catches attacks and malware calling home, shows who each app talks to", soon: true },
        "AI triage: every alert explained, with a quarantine or block button you approve",
        "The cyborg warden pet, with its own pane in your menu bar or tray",
        "5 computers",
      ],
    },
  },
  plans: [
    { id: 1, tier: "ram", interval: "2mo", usd: 4 },
    { id: 2, tier: "pro", interval: "2mo", usd: 5 },
    { id: 3, tier: "ram", interval: "year", usd: 20 },
    { id: 4, tier: "pro", interval: "year", usd: 24 },
  ],
};

// Finds a plan by tier and interval.
export function planFor(plans, tier, interval) {
  return plans.find((p) => p.tier === tier && p.interval === interval) || null;
}

// What a year of the annual plan saves against six 2-month payments, in whole percent.
export function annualSaving(plans, tier) {
  const m = planFor(plans, tier, "2mo");
  const y = planFor(plans, tier, "year");
  if (!m || !y) return 0;
  return Math.round((1 - y.usd / (m.usd * PER_YEAR["2mo"])) * 100);
}

// Billing periods (owner's decision, 2026-11-02): every 2 months, or yearly.
// The contract and the card subscription charge on the same 60-day cycle.
export const PERIOD = { "2mo": "2 months", year: "year" };
export const PER_YEAR = { "2mo": 6, year: 1 };
