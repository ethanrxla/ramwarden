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

  // The wardens: one agent per job, each with its own icon. The landing page
  // builds its "Meet the wardens" cards from this list, so adding a warden is
  // one entry here plus an icon in assets/. `tier` is "included" (every plan),
  // "addon" (bought on top of Pro) or "pro"; `status` is "available" or "soon".
  wardens: [
    {
      id: "ram",
      name: "RAM Warden",
      icon: "assets/warden-ram.svg",
      job: "Memory",
      tier: "included",
      status: "available",
      summary:
        "Watches memory pressure, tries reversible fixes first and protects what you're using. Jev decides which idle apps and tabs you won't miss.",
    },
    {
      id: "wazuh",
      name: "Wazuh Warden",
      icon: "assets/warden-threat.svg",
      job: "Threat detection",
      tier: "addon",
      status: "soon",
      summary:
        "Built on the open-source Wazuh agent: file integrity, suspicious processes, vulnerable software and known attack patterns, explained in plain words.",
    },
  ],

  plans: [
    { id: "pro_30d", name: "Pro, 30 days", usd: 4, days: 30 },
    { id: "pro_365d", name: "Pro, 1 year", usd: 36, days: 365 },
  ],
  limits: {
    free: { perDay: 5, devices: 1 },
    pro: { perDay: 200, devices: 3 },
  },
};
