// A stand-in server and wallet for previewing the pages before the hosted API
// is live, and for the browser tests. Loaded only when the URL has `?mock=1`;
// nothing in it ever touches the network or a real wallet.
//
// It follows docs/specs/website.md, so a page that works against it works
// against the real server, and a contract change shows up here first.

import { ApiError, normaliseUserCode } from "./api.js";

const DAY = 86_400_000;

// `?mock=1&as=trial|active|lapsed` starts already signed in, in that state,
// so the landing page's install box can be previewed signed in.
const AS = typeof location === "undefined" ? null : new URLSearchParams(location.search).get("as");

export class MockApi {
  constructor() {
    this.session = null;
    this.pendingCodes = new Map([["BCDF-GHJK", { device_name: "ethan-desktop", platform: "linux" }]]);
    this.account_ = null;
    this.payments = new Map();
    if (["trial", "active", "lapsed"].includes(AS)) {
      this.account_ = this.#newAccount("evm", "0x8ba1f109551bD432803012645Ac136ddd64DBA72");
      Object.assign(this.account_, {
        tier: "pro",
        interval: "month",
        status: AS,
        trial_ends_at: AS === "trial" ? new Date(Date.now() + 14 * DAY).toISOString() : null,
        paid_until: AS === "active" ? new Date(Date.now() + 30 * DAY).toISOString() : AS === "lapsed" ? new Date(Date.now() - 3 * DAY).toISOString() : null,
      });
      this.session = "mock-session";
    }
  }

  #newAccount(chain, address) {
    return {
      id: "acc_mock",
      identity: { kind: "wallet", chain, address },
      tier: null,
      interval: null,
      status: null,
      trial_ends_at: null,
      paid_until: null,
      device_limit: 5,
      prefs: {},
      devices: [{ id: "dev_1", name: "ethan-desktop", platform: "linux", last_seen_at: new Date().toISOString() }],
    };
  }

  async lookupDevice(userCode) {
    const code = normaliseUserCode(userCode);
    const d = code && this.pendingCodes.get(code);
    if (!d) throw new ApiError(404, "That code has expired or was already used. Start sign-in again from the app.");
    return { status: "pending", ...d, expires_at: new Date(Date.now() + 10 * 60_000).toISOString() };
  }

  async challenge({ chain, address, userCode }) {
    const lines = [
      "ramwarden.app wants you to sign in with your " + (chain === "evm" ? "Ethereum" : "Solana") + " account:",
      address,
      "",
      userCode ? `Link the device with code ${userCode} to your RamWarden account.` : "Sign in to your RamWarden account.",
      "",
      "URI: https://ramwarden.app",
      "Version: 1",
      "Nonce: mock" + Math.random().toString(36).slice(2, 10),
      "Issued At: " + new Date().toISOString(),
    ];
    if (userCode) lines.push("Resources:", `- urn:ramwarden:device:${userCode}`);
    return { message: lines.join("\n"), expires_at: new Date(Date.now() + 5 * 60_000).toISOString() };
  }

  async verify({ chain, address, message }) {
    const linked = message.match(/urn:ramwarden:device:([A-Z]{4}-[A-Z]{4})/)?.[1] || null;
    if (linked) this.pendingCodes.delete(linked);
    this.account_ ??= this.#newAccount(chain, address);
    this.session = "mock-session";
    return { session: this.session, account: this.account_, linked_device: linked ? this.account_.devices[0] : null };
  }

  async account() {
    if (!this.session) throw new ApiError(401, "Sign in first.");
    return this.account_;
  }
  async savePrefs(prefs) {
    if (!this.session) throw new ApiError(401, "Sign in first.");
    this.account_.prefs = { ...this.account_.prefs, ...prefs };
    return { ok: true };
  }
  async installLink() {
    if (!this.session) throw new ApiError(401, "Sign in first.");
    if (!["trial", "active", "grace"].includes(this.account_.status)) throw new ApiError(402, "Start a trial or renew to get the installer.");
    const token = "K7Q2-9MXA";
    return {
      expires_at: new Date(Date.now() + 30 * 60_000).toISOString(),
      commands: {
        macos: `curl -fsSL https://get.ramwarden.app/i/${token} | sh`,
        linux: `curl -fsSL https://get.ramwarden.app/i/${token} | sh`,
        windows: `irm https://get.ramwarden.app/i/${token}/ps | iex`,
      },
      installers: {
        macos: `https://get.ramwarden.app/i/${token}/RamWarden.pkg`,
        windows: `https://get.ramwarden.app/i/${token}/RamWarden.msi`,
        linux: `https://get.ramwarden.app/i/${token}/ramwarden.deb`,
      },
    };
  }
  async revokeDevice(id) {
    this.account_.devices = this.account_.devices.filter((d) => d.id !== id);
    return { ok: true };
  }
  signOut() {
    this.session = null;
  }

  async checkoutConfig() {
    return {
      // docs/product-spec.md 1.2; the test network sells every plan.
      plans: [
        { id: 1, tier: "ram", interval: "month", usd: 4 },
        { id: 2, tier: "pro", interval: "month", usd: 5 },
        { id: 3, tier: "ram", interval: "year", usd: 20 },
        { id: 4, tier: "pro", interval: "year", usd: 24 },
      ],
      trial_days: 14,
      base: { chain_id: 84532, network: "Base Sepolia (test)" },
      solana: { network: "Solana devnet (test)" },
      stripe: true,
    };
  }
  async checkoutBase(plan) {
    return this.#payment(plan, "base_usdc", {
      chain_id: 84532,
      transactions: [
        { to: "0x0000000000000000000000000000000000000001", data: "0x095ea7b3" },
        { to: "0x0000000000000000000000000000000000000002", data: "0x12345678" },
      ],
    });
  }
  async checkoutSolana(plan) {
    return this.#payment(plan, "solana_pay", { message: "3Bxs4Bc3VYuGVB19" });
  }
  async checkoutStripe() {
    return { url: "#stripe-mock" };
  }
  async payment(id) {
    const p = this.payments.get(id);
    if (!p) throw new ApiError(404, "No such payment.");
    // Confirms on the second look, like a payment that needed one more block.
    if (p.status === "pending" && ++p.polls >= 2) {
      p.status = "confirmed";
      // Confirming the approval starts the trial; nothing is charged yet.
      const plan = (await this.checkoutConfig()).plans.find((x) => x.id === p.plan);
      Object.assign(this.account_, {
        tier: plan.tier,
        interval: plan.interval,
        status: "trial",
        trial_ends_at: new Date(Date.now() + 14 * DAY).toISOString(),
      });
    }
    return p;
  }

  #payment(plan, method, extra) {
    const id = "pay_" + (this.payments.size + 1);
    this.payments.set(id, { id, plan, method, status: "pending", polls: 0 });
    return { payment_id: id, ...extra };
  }
}

// A wallet that signs instantly, so the flow can be clicked through with no
// extension installed. Only ever offered in mock mode.
export class DemoWallet {
  constructor(chain) {
    this.chain = chain;
    this.name = chain === "evm" ? "Demo wallet (EVM)" : "Demo wallet (Solana)";
    this.address = null;
  }
  async connect() {
    this.address = this.chain === "evm" ? "0x8ba1f109551bD432803012645Ac136ddd64DBA72" : "7EcDhSYGxXyscszYEp35KHN8vvw3svAuLKTzXwCFLtV";
    return this.address;
  }
  async sign() {
    return this.chain === "evm" ? "0x" + "ab".repeat(65) : "5".repeat(88);
  }
  async pay() {
    return ["0xmock"];
  }
}
