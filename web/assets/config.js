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
  apiBase: typeof location !== "undefined" && ["localhost", "127.0.0.1"].includes(location.hostname)
    ? location.origin
    : typeof location !== "undefined" && location.hostname === "staging.ramwarden.com"
      ? "https://api-staging.ramwarden.com" : "https://api.ramwarden.com",

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
  // the page. Bullets describe the shipped behavior in everyday language
  // (README, docs/specs/browser-memory.md, threatwarden.md).
  // `soon` marks a part that is specified but not shipped yet.
  trialDays: 14,
  lite: {
    name: "RamWarden Lite",
    tagline: "Keep it tidy.",
    cta: "Get it free",
    features: [
      "An enhanced memory algorithm that tracks which apps are active",
      "Frees memory in stages, starting with changes that are easy to undo",
      "A configurable tab budget, with a reopen list for tabs Warden closes",
      "Protects active work, games, calls, terminals and virtual machines",
      "Resumes paused apps when memory pressure eases",
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
        "A customizable AI companion for chat and memory advice",
        "Ranks idle apps by how likely you are to return, then pauses or closes those you're unlikely to need when your settings allow it",
        "AI-guided tab cleanup, with a reopen list for tabs it closes",
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
        "Security checks for changed files, suspicious activity and vulnerable software",
        { text: "Malware scans when your computer has room to spare, plus scan on demand", soon: true },
        { text: "Network alerts and a view of which apps connect to the internet", soon: true },
        "Explains security findings; quarantine and blocking are planned",
        "Your cyborg ThreatWarden companion",
        "5 computers",
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

// What a year of the annual plan saves against twelve monthly payments, in whole percent.
export function annualSaving(plans, tier) {
  const m = planFor(plans, tier, "month");
  const y = planFor(plans, tier, "year");
  if (!m || !y) return 0;
  return Math.round((1 - y.usd / (m.usd * PER_YEAR["month"])) * 100);
}

// Monthly and annual pricing (owner decision, 2026-11-02).
export const PERIOD = { "month": "month", year: "year" };
export const PER_YEAR = { "month": 12, year: 1 };
