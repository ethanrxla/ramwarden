// A stand-in server and wallet for previewing the pages before the hosted API
// is live, and for the browser tests. Loaded only when the URL has `?mock=1`;
// nothing in it ever touches the network or a real wallet.
//
// It follows docs/specs/website.md, so a page that works against it works
// against the real server, and a contract change shows up here first.

import { ApiError, normaliseUserCode } from "./api.js";

const DAY = 86_400_000;

// `?mock=1&as=lite|trial|active|lapsed` starts already signed in, in that state,
// so the landing page's install box can be previewed signed in. `signedIn`
// stands for the server's HttpOnly cookie, which a real page never sees.
const AS = typeof location === "undefined" ? null : new URLSearchParams(location.search).get("as");

export class MockApi {
  constructor() {
    this.signedIn = false;
    this.emailCodes = new Map();
    this.pendingCodes = new Map([["BCDF-GHJK", { device_name: "ethan-desktop", platform: "linux" }]]);
    this.account_ = null;
    this.payments = new Map();
    if (AS === "lite") {
      this.account_ = this.#newAccount("email", "you@example.com");
      this.signedIn = true;
    }
    if (["trial", "active", "lapsed"].includes(AS)) {
      this.account_ = this.#newAccount("evm", "0x8ba1f109551bD432803012645Ac136ddd64DBA72");
      Object.assign(this.account_, {
        tier: AS === "trial" ? "ram" : "pro", // only Plus has a trial
        interval: "month",
        status: AS,
        device_limit: AS === "trial" ? 3 : 5,
        trial_ends_at: AS === "trial" ? new Date(Date.now() + 14 * DAY).toISOString() : null,
        paid_until: AS === "active" ? new Date(Date.now() + 30 * DAY).toISOString() : AS === "lapsed" ? new Date(Date.now() - 3 * DAY).toISOString() : null,
      });
      this.signedIn = true;
    }
  }

  #newAccount(chain, address) {
    return {
      id: "acc_mock",
      identity: chain === "email" ? { kind: "email", email: address } : { kind: "wallet", chain, address },
      trial_available: true,
      billing: null,
      tier: null,
      interval: null,
      status: null,
      trial_ends_at: null,
      paid_until: null,
      device_limit: 1, // Lite until a plan is bought
      prefs: {},
      devices: [{ id: "dev_1", name: "ethan-desktop", platform: "linux", last_seen_at: new Date().toISOString() }],
    };
  }

  async approveDevice(userCode) {
    if (!this.signedIn || !this.pendingCodes.has(userCode)) throw new ApiError(401,"Sign in and check the code.");
    this.pendingCodes.delete(userCode);return {approved:true};
  }

  async lookupDevice(userCode) {
    const code = normaliseUserCode(userCode);
    const d = code && this.pendingCodes.get(code);
    if (!d) throw new ApiError(404, "That code has expired or was already used. Start sign-in again from the app.");
    return { status: "pending", ...d, expires_at: new Date(Date.now() + 10 * 60_000).toISOString() };
  }

  async challenge({ chain, address, userCode, link = false }) {
    const lines = [
      "ramwarden.app wants you to sign in with your " + (chain === "evm" ? "Ethereum" : "Solana") + " account:",
      address,
      "",
      link ? "Link this wallet as a sign-in method. No payment." : userCode ? `Link the device with code ${userCode} to your RamWarden account.` : "Sign in to your RamWarden account.",
      "",
      "URI: https://ramwarden.app",
      "Version: 1",
      "Nonce: mock" + Math.random().toString(36).slice(2, 10),
      "Issued At: " + new Date().toISOString(),
    ];
    if (userCode) lines.push("Resources:", `- urn:ramwarden:device:${userCode}`);
    return { message: lines.join("\n"), expires_at: new Date(Date.now() + 5 * 60_000).toISOString() };
  }

  async verify({ chain, address, message, link = false }) {
    const linked = message.match(/urn:ramwarden:device:([A-Z]{4}-[A-Z]{4})/)?.[1] || null;
    if (linked) this.pendingCodes.delete(linked);
    this.account_ ??= this.#newAccount(chain, address);
    if (link) {
      this.account_.identities ||= [this.account_.identity];
      this.account_.identities.push({kind:"wallet",chain,address});
    }
    this.signedIn = true;
    return { account: this.account_, linked_identity:link, linked_device: linked ? this.account_.devices[0] : null };
  }

  // Email sign-in: the code is always 123456 in preview.
  async emailStart({ email, userCode, link = false }) {
    if (!/^[^@\s]+@[^@\s]+\.[^@\s]+$/.test(email || "")) throw new ApiError(400, "That doesn't look like an email address.");
    this.emailCodes.set(email.toLowerCase(), { code: "123456", userCode: userCode ?? null, link });
    return { sent: true, expires_at: new Date(Date.now() + 10 * 60_000).toISOString() };
  }
  async emailVerify({ email, code, link = false }) {
    const e = this.emailCodes.get((email || "").toLowerCase());
    if (!e || e.link !== link || e.code !== String(code).trim()) throw new ApiError(400, "That code is wrong or has expired. Check the email, or send a new one.");
    this.emailCodes.delete(email.toLowerCase());
    if (e.userCode) this.pendingCodes.delete(e.userCode);
    this.account_ ??= this.#newAccount("email", email);
    if (link) {
      this.account_.identities ||= [this.account_.identity];
      this.account_.identities.push({kind:"email",email});
    }
    this.signedIn = true;
    return { account: this.account_, linked_device: e.userCode ? this.account_.devices[0] : null };
  }
  async logout() {
    this.signedIn = false;
    return { ok: true };
  }

  async account() {
    if (!this.signedIn) throw new ApiError(401, "Sign in first.");
    return this.account_;
  }
  async savePrefs(prefs) {
    if (!this.signedIn) throw new ApiError(401, "Sign in first.");
    this.account_.prefs = prefs;
    return { prefs };
  }
  async usageStatus() {
    if (!this.signedIn) throw new ApiError(401, "Sign in first.");
    return { enabled: this.account_.prefs?.share_usage === true };
  }
  async installLink() {
    if (!this.signedIn) throw new ApiError(401, "Sign in first.");
    // Lite (signed in, nothing bought) gets a line too; the device limit of 1
    // is the server's to enforce.
    if (this.account_.status && !["trial", "active", "grace"].includes(this.account_.status)) throw new ApiError(402, "Renew to get the installer.");
    const token = "K7Q2-9MXA";
    return {
      expires_at: new Date(Date.now() + 30 * 60_000).toISOString(),
      commands: {
        macos: `curl -fsSL https://get.ramwarden.app/i/${token} | sh`,
        linux: `curl -fsSL https://get.ramwarden.app/i/${token} | sh`,
        windows: `irm https://get.ramwarden.app/i/${token}/ps | iex`,
      },
      installers: {
        macos: `https://get.ramwarden.app/i/${token}/RamWarden.dmg`,
        windows: `https://get.ramwarden.app/i/${token}/RamWarden.msi`,
        linux: `https://get.ramwarden.app/i/${token}/ramwarden.deb`,
      },
    };
  }
  async revokeDevice(id) {
    this.account_.devices = this.account_.devices.filter((d) => d.id !== id);
    return { ok: true };
  }
  async cancel() {
    const a = this.account_;
    if (!a?.status || !["trial", "active", "grace"].includes(a.status)) throw new ApiError(409, "There is nothing to cancel.");
    if (a.billing?.method === "base") {
      a.status = "cancelled";
      return { chain_id: 84532, payer: a.billing.payer, transactions: [{ to: "0x0000000000000000000000000000000000000002", data: "0x40e58ee5", value: "0x0" }] };
    }
    a.status = "cancelled";
    return { cancelled: true, service_until: a.trial_ends_at || a.paid_until };
  }

  async checkoutConfig() {
    return {
      // docs/product-spec.md 1.2; the test network sells every plan.
      plans: [
        { id: 1, tier: "ram", name: "RamWarden Plus", usd: 4, interval: "month", days: 60, trial_days: 14, approval_usd: 24, wardens: ["ram"] },
        { id: 2, tier: "pro", name: "RamWarden Pro", usd: 5, interval: "month", days: 60, trial_days: 0, approval_usd: 30, wardens: ["ram", "threat"] },
        { id: 3, tier: "ram", name: "RamWarden Plus", usd: 20, interval: "year", days: 365, trial_days: 14, approval_usd: 20, wardens: ["ram"] },
        { id: 4, tier: "pro", name: "RamWarden Pro", usd: 24, interval: "year", days: 365, trial_days: 0, approval_usd: 24, wardens: ["ram", "threat"] },
      ],
      base: { chain_id: 84532, network: "Base Sepolia (test)" },
      solana: { network: "Solana devnet (test)" },
      stripe: true,
    };
  }
  async checkoutBase(plan, payer) {
    const p = (await this.checkoutConfig()).plans.find((x) => x.id === plan);
    this.payer = payer;
    if (this.#upgrading(p)) {
      // An upgrade changes the running plan; it must come from the wallet that pays.
      const payerNow = this.account_.billing?.payer || payer;
      return this.#payment(plan, "base", {
        upgrade: true,
        payer: payerNow,
        trial: false,
        transactions: [{ to: "0x0000000000000000000000000000000000000002", data: "0x87654321" }],
      });
    }
    return this.#payment(plan, "base", {
      chain_id: 84532,
      trial: this.account_.trial_available && p.trial_days > 0,
      approval_usd: p.approval_usd,
      transactions: [
        { to: "0x0000000000000000000000000000000000000001", data: "0x095ea7b3" },
        { to: "0x0000000000000000000000000000000000000002", data: "0x12345678" },
      ],
    });
  }
  async checkoutSolana(plan) {
    return this.#payment(plan, "solana_pay", { message: "3Bxs4Bc3VYuGVB19" });
  }
  async checkoutStripe(plan) {
    const p = (await this.checkoutConfig()).plans.find((x) => x.id === plan);
    // A card upgrade changes the subscription in place: no checkout page.
    if (this.#upgrading(p)) return this.#payment(plan, "card", { upgrade: true, url: null, trial: false });
    return this.#payment(plan, "card", { url: "#stripe-mock", trial: this.account_.trial_available && p.trial_days > 0 });
  }
  async payment(id) {
    const p = this.payments.get(id);
    if (!p) throw new ApiError(404, "No such payment.");
    // Confirms on the second look, like a payment that needed one more block.
    if (p.status === "pending" && ++p.polls >= 2) {
      p.status = "confirmed";
      // Confirming the approval starts the trial; nothing is charged yet.
      const plan = (await this.checkoutConfig()).plans.find((x) => x.id === p.plan);
      // Only Plus has a trial; Pro is charged straight away.
      const trial = this.account_.trial_available && plan.trial_days > 0;
      Object.assign(this.account_, {
        tier: plan.tier,
        interval: plan.interval,
        status: trial ? "trial" : "active",
        trial_ends_at: trial ? new Date(Date.now() + 14 * DAY).toISOString() : null,
        paid_until: trial ? null : new Date(Date.now() + plan.days * DAY).toISOString(),
        trial_available: trial ? false : this.account_.trial_available,
        device_limit: plan.tier === "pro" ? 5 : 3, // Plus 3, Pro 5
        billing: { method: p.method, payer: this.payer },
      });
      Object.assign(p, { account_status: this.account_.status, tier: plan.tier, trial_ends_at: this.account_.trial_ends_at });
    }
    return p;
  }

  // The support desk, cut down: a few of the server's answers, keyword
  // routed the way its fallback is, so the chat can be clicked through.
  supportHistory({ conversation, warden }) {
    const c = this.#support?.get(conversation);
    return Promise.resolve({
      greeting: warden === "threat" ? "I'm ThreatWarden's warden. Ask me about security, plans or setup." : "I'm your warden. Ask me about plans, the free trial, installing, or how I free memory.",
      chips: [
        { topic: "plans", label: "Which plan is right for me?" },
        { topic: "trial", label: "How does the free trial work?" },
        { topic: "human", label: "Talk to a person" },
      ],
      live: Boolean(c),
      thread: c ? { warden, messages: c.messages, ticket: c.ticket } : null,
    });
  }

  supportChat({ conversation, warden, message, topic }) {
    this.#support ??= new Map();
    const text = message || { plans: "Which plan is right for me?", trial: "How does the free trial work?", human: "Talk to a person" }[topic] || "";
    if (!text.trim()) return Promise.reject(new ApiError(422, "Ask me something first."));
    let fresh = null;
    if (!this.#support.has(conversation)) {
      fresh = conversation = "rwsc_mock" + (this.#support.size + 1);
      this.#support.set(conversation, { messages: [], ticket: null });
    }
    const c = this.#support.get(conversation);
    const q = text.toLowerCase();
    const pick = topic || (/price|cost|how much/.test(q) ? "pricing" : /trial/.test(q) ? "trial" : /plan|tier/.test(q) ? "plans" : /human|person|broken|not working|refund/.test(q) ? "human" : "unknown");
    const answers = {
      plans: "There are three plans. Lite is free on 1 computer, Plus adds the AI warden pet on 3 computers ($4 per month or $20 a year, 14-day trial), and Pro adds ThreatWarden on 5 ($5 per month or $24 a year).",
      pricing: "Lite is free. Plus is $4 per month or $20 a year. Pro is $5 per month or $24 a year.",
      trial: "RamWarden Plus is free for 14 days. After that, your selected payment method is charged $4 per month or $20 per year, depending on your plan, until you cancel. Pro is $5 per month or $24 per year, paid from day one.",
      human: "I'll get a person on it. Open a ticket below, with an email if you'd like a reply there.",
      unknown: "I don't know that one for sure, and I'd rather not guess. I can open a ticket so a person on the team answers you.",
    };
    const at = Math.floor(Date.now() / 1000);
    c.messages.push({ from: "visitor", text, at }, { from: "warden", text: answers[pick], at });
    return Promise.resolve({
      conversation: fresh,
      reply: { text: answers[pick], topic: pick, chips: pick === "plans" ? [{ topic: "trial", label: "How does the free trial work?" }] : [], offer_ticket: ["human", "unknown"].includes(pick) && !c.ticket },
      ticket: c.ticket,
    });
  }

  supportTicket({ conversation, email }) {
    const c = this.#support?.get(conversation);
    if (!c) return Promise.reject(new ApiError(404, "That chat has expired."));
    c.ticket ??= { reference: "RW-BCD234", status: "open", subject: c.messages.at(-2)?.text || "Question", opened_at: Math.floor(Date.now() / 1000) };
    const text = email ? `Ticket ${c.ticket.reference} is open. The team will write back to ${email}, and the reply shows here too.` : `Ticket ${c.ticket.reference} is open. Come back to this chat for the team's reply.`;
    c.messages.push({ from: "warden", text, at: c.ticket.opened_at });
    return Promise.resolve({ ticket: c.ticket, reply: { text } });
  }

  #support = null;

  // Plus to Pro (or monthly to annual) while a plan is live.
  #upgrading(plan) {
    const a = this.account_;
    return ["trial", "active", "grace"].includes(a.status) && a.tier === "ram" && plan.tier === "pro";
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
