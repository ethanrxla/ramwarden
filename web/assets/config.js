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
    // The release workflow publishes these names (docs/specs/updates.md):
    windows: null, // ".../releases/latest/download/ramwarden-windows-x86_64.exe"
    macos: null,   // ".../releases/latest/download/RamWarden.pkg"
    linux: null,   // ".../releases/latest/download/ramwarden-linux-x86_64.deb"
  },

  // Chrome Web Store listing. Null until the store review passes; until then
  // the page explains how to load the extension unpacked.
  chromeStore: null,

  plans: [
    { id: "pro_30d", name: "Pro, 30 days", usd: 4, days: 30 },
    { id: "pro_365d", name: "Pro, 1 year", usd: 36, days: 365 },
  ],
  limits: {
    free: { perDay: 5, devices: 1 },
    pro: { perDay: 200, devices: 3 },
  },
};
