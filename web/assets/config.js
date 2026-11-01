// Site settings. Everything a deploy might change lives here, so the pages
// themselves never need editing to move the API, change a price or publish a
// release.
//
// The prices and quotas below are the ones in the backend design and the
// `ramwarden-cloud` tier limits (free 5 a day and 1 device, Pro 200 a day and
// 3 devices). The server is the authority: when `/v1/checkout/config`
// answers, its plans replace these, so a price change on the server cannot
// leave the site advertising the old one.

export const config = {
  // The hosted API (`ramwarden-api`). Changing it also means changing the
  // `connect-src` in each page's Content-Security-Policy.
  apiBase: "https://api.ramwarden.app",

  repo: "https://github.com/ethanrxla/ramwarden",
  releases: "https://github.com/ethanrxla/ramwarden/releases/latest",

  // Direct download links per platform. While one is null its button goes to
  // the releases page instead, so publishing a release never breaks the site
  // and an unpublished asset never shows a 404.
  downloads: {
    windows: null, // e.g. ".../releases/latest/download/ramwarden-setup-x64.exe"
    macos: null,   // e.g. ".../releases/latest/download/RamWarden.dmg"
    linux: null,   // e.g. ".../releases/latest/download/ramwarden_0.2.1_amd64.deb"
  },

  // Chrome Web Store listing. Null until the store review passes; until then
  // the page explains how to load the extension unpacked.
  chromeStore: null,

  // Each warden is its own prepaid pass. The server's /v1/checkout/config
  // wins on the account page; these fill the landing pages. `soon` plans are
  // shown but not sold until the warden's hosted tier is live.
  plans: [
    // Ids are the checkout contract's on-chain plan ids (contracts/plans.json).
    { id: 1, name: "RamWarden Pro, 2 months", usd: 4, days: 60, wardens: ["ram"] },
    { id: 2, name: "ThreatWarden, 2 months", usd: 6, days: 60, wardens: ["threat"], soon: true },
    { id: 3, name: "RamWarden Pro + ThreatWarden, 2 months", usd: 10, days: 60, wardens: ["ram", "threat"], soon: true },
  ],
  limits: {
    free: { perDay: 5, devices: 1 },
    pro: { perDay: 200, devices: 3 },
  },
};
