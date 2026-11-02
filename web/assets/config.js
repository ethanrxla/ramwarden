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

  // Two tiers, each monthly or yearly, both with a 14-day trial
  // (docs/product-spec.md 1.2). Ids are the checkout contract's plan ids.
  trialDays: 14,
  tiers: {
    ram: { name: "RamWarden", blurb: "AI-powered memory management and system resource optimization.", devices: 5 },
    pro: { name: "RamWarden Pro", blurb: "Everything in RamWarden, plus the ThreatWarden security companion.", devices: 5 },
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
