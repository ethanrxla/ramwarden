// /account: sign in on the web, see the pass and today's usage, revoke
// devices, and buy or extend Pro.
//
// Payments only ever extend `entitlement_until` on the server, whichever way
// they are made, so this page shows one date and never needs to know how the
// last pass was paid for. A pass bought early stacks on the current one.

import { config } from "./config.js";
import { errorText, formatDate, h, isMock, makeApi, mockBanner, notice, pickWallet, shortAddress, show, signIn } from "./common.js";

const PLATFORM = { linux: "Linux", windows: "Windows", macos: "macOS" };
const POLL_MS = isMock ? 400 : 3000;
const POLL_LIMIT = 60; // three minutes: Base confirms in seconds, Solana faster

const root = document.getElementById("flow");
mockBanner();
const api = await makeApi();

class AccountPage {
  constructor() {
    this.account = null;
    this.plans = config.plans;
    this.methods = { base: true, solana: true, stripe: true };
  }

  async start() {
    if (api.session) {
      try {
        this.account = await api.account();
      } catch (err) {
        if (err.status !== 401) {
          show(root, h("h1", {}, "Your account"), notice("error", errorText(err)));
          return;
        }
      }
    }
    if (!this.account) await this.signIn();
    await this.loadCheckout();
    this.render();
    if (new URLSearchParams(location.search).get("paid") === "stripe") {
      this.flash(notice("ok", "Card payment received. Your pass updates as soon as Stripe confirms it, usually within a minute."));
    }
  }

  async signIn() {
    const area = h("div");
    const problem = h("div");
    show(
      root,
      h("h1", {}, "Your account"),
      h("p", {}, "Sign in with the wallet you used in the app, or any wallet to make a new account."),
      problem,
      area,
    );
    for (;;) {
      const wallet = await pickWallet(area, { purpose: "You will sign a short message. It costs no gas and moves no funds." });
      show(area, notice("info", `Check ${wallet.name} for the sign-in message.`));
      try {
        this.account = (await signIn(api, wallet)).account;
        return;
      } catch (err) {
        // Back to the picker with the reason shown above it.
        show(problem, notice("error", errorText(err)));
      }
    }
  }

  // The server's plans and enabled methods win over the defaults in config.js.
  async loadCheckout() {
    try {
      const c = await api.checkoutConfig();
      if (Array.isArray(c.plans) && c.plans.length) this.plans = c.plans;
      this.methods = { base: !!c.base, solana: !!c.solana, stripe: !!c.stripe };
      this.networks = { base: c.base?.network, solana: c.solana?.network };
    } catch {
      this.methods = { base: false, solana: false, stripe: false };
    }
  }

  flash(node) {
    this.flashArea?.replaceChildren(node);
  }

  render() {
    const a = this.account;
    const pro = a.tier === "pro" && a.entitlement_until && Date.parse(a.entitlement_until) > Date.now();
    const limits = pro ? config.limits.pro : config.limits.free;
    this.flashArea = h("div");

    show(
      root,
      h("h1", {}, "Your account"),
      this.flashArea,
      h(
        "section",
        { class: "panel" },
        h("h2", {}, pro ? "RamWarden Pro" : "Free"),
        pro
          ? h("p", {}, `Your pass runs until ${formatDate(a.entitlement_until)}. Buying another adds to that date.`)
          : h("p", {}, `${limits.perDay} hosted Jev decisions a day on ${limits.devices} device. Local decisions are always unlimited.`),
        // Each warden's own pass, as the server reports it.
        Object.entries(a.entitlements || {})
          .filter(([, until]) => Date.parse(until) > Date.now())
          .map(([slug, until]) => h("p", {}, `${slug === "threat" ? "ThreatWarden" : slug === "ram" ? "RamWarden Pro" : slug} runs until ${formatDate(until)}.`)),
        typeof a.used_today === "number" ? h("p", { class: "muted" }, `Hosted decisions used today: ${a.used_today} of ${limits.perDay}.`) : null,
        h("p", { class: "muted" }, "Signed in with ", (a.wallets || []).map((w) => shortAddress(w.address)).join(", ") || "a wallet", "."),
        h("button", { class: "button secondary small", type: "button", onclick: () => { api.signOut(); location.reload(); } }, "Sign out"),
      ),
      this.devicesPanel(limits),
      this.buyPanel(pro),
    );
  }

  devicesPanel(limits) {
    const devices = this.account.devices || [];
    return h(
      "section",
      { class: "panel", id: "devices" },
      h("h2", {}, "Devices"),
      h("p", { class: "muted" }, `${devices.length} of ${limits.devices} in use. Revoke a machine you no longer use, or one you have lost.`),
      devices.length === 0
        ? h("p", {}, "No devices yet. Click Sign in inside RamWarden to link this computer.")
        : h(
            "ul",
            { class: "devices" },
            devices.map((d) =>
              h(
                "li",
                {},
                h("div", {}, h("strong", {}, d.name || "Unnamed device"), h("span", { class: "muted" }, ` ${PLATFORM[d.platform] || d.platform || ""}`, d.last_seen_at ? `, last seen ${formatDate(d.last_seen_at)}` : "")),
                h(
                  "button",
                  {
                    class: "button secondary small",
                    type: "button",
                    onclick: async (e) => {
                      if (!confirm(`Revoke ${d.name || "this device"}? It falls back to local decisions until you sign in on it again.`)) return;
                      e.target.disabled = true;
                      try {
                        await api.revokeDevice(d.id);
                        this.account.devices = devices.filter((x) => x.id !== d.id);
                        this.render();
                      } catch (err) {
                        e.target.disabled = false;
                        this.flash(notice("error", errorText(err)));
                      }
                    },
                  },
                  "Revoke",
                ),
              ),
            ),
          ),
    );
  }

  buyPanel(pro) {
    const any = this.methods.base || this.methods.solana || this.methods.stripe;
    const area = h("div");
    // A warden whose hosted tier is not live yet is shown on the landing page
    // but never sold here.
    const plans = this.plans.filter((p) => !p.soon);
    let plan = plans[0]?.id;
    const planButtons = plans.map((p) =>
      h(
        "label",
        { class: "plan-choice" },
        h("input", { type: "radio", name: "plan", value: p.id, checked: p.id === plan, onchange: () => (plan = p.id) }),
        h("strong", {}, p.name),
        h("span", {}, `$${p.usd} USDC`),
      ),
    );
    const method = (key, label, sub, run) =>
      this.methods[key] ? h("button", { class: "wallet", type: "button", onclick: () => run(plan, area) }, h("strong", {}, label), h("span", {}, sub)) : null;

    return h(
      "section",
      { class: "panel", id: "buy" },
      h("h2", {}, pro ? "Extend Pro" : "Get Pro"),
      h("p", {}, `${config.limits.pro.perDay} hosted Jev decisions a day on up to ${config.limits.pro.devices} devices. A prepaid pass, not a subscription: nothing renews or charges you later.`),
      any
        ? [
            h("div", { class: "plans" }, planButtons),
            h(
              "div",
              { class: "wallets" },
              method("base", "USDC on Base", this.networks?.base || "MetaMask, Phantom or any EVM wallet", (p, a) => this.payCrypto("evm", p, a)),
              method("solana", "USDC on Solana", this.networks?.solana || "Phantom", (p, a) => this.payCrypto("solana", p, a)),
              method("stripe", "Card", "Stripe checkout", (p, a) => this.payStripe(p, a)),
            ),
            area,
          ]
        : notice("info", "Checkout opens soon. RamWarden's local decisions are free and unlimited in the meantime."),
    );
  }

  async payCrypto(chain, plan, area) {
    const wallet = await pickWallet(area, {
      chain,
      purpose: "Pick the wallet to pay from. It does not have to be the one you signed in with.",
    });
    try {
      show(area, notice("info", "Preparing the payment…"));
      const order = chain === "evm" ? await api.checkoutBase(plan, wallet.address) : await api.checkoutSolana(plan, wallet.address);
      show(area, notice("info", chain === "evm" ? "Confirm in your wallet. There are two steps: allow the USDC amount, then pay." : "Confirm the payment in Phantom."));
      await wallet.pay(order);
      await this.waitForPayment(order.payment_id, area);
    } catch (err) {
      show(area, notice("error", errorText(err)));
    }
  }

  async payStripe(plan, area) {
    try {
      show(area, notice("info", "Opening Stripe checkout…"));
      const { url } = await api.checkoutStripe(plan);
      if (isMock) {
        show(area, notice("info", "Preview mode: Stripe checkout would open here."));
        return;
      }
      if (!/^https:\/\/checkout\.stripe\.com\//.test(url)) throw new Error("Unexpected checkout address.");
      location.assign(url);
    } catch (err) {
      show(area, notice("error", errorText(err)));
    }
  }

  async waitForPayment(id, area) {
    show(area, notice("info", "Payment sent. Waiting for the network to confirm it…"));
    for (let i = 0; i < POLL_LIMIT; i++) {
      await new Promise((r) => setTimeout(r, POLL_MS));
      const p = await api.payment(id);
      if (p.status === "confirmed") {
        this.account = await api.account();
        this.render();
        this.flash(notice("ok", `Thank you. Pro now runs until ${formatDate(this.account.entitlement_until)}.`));
        return;
      }
      if (p.status === "failed") {
        show(area, notice("error", p.reason || "The payment did not go through. No pass was added, and a failed transaction moves no USDC."));
        return;
      }
    }
    show(area, notice("info", "Still waiting for confirmation. It is safe to close this page: your pass is added as soon as the payment confirms."));
  }
}

new AccountPage().start();
