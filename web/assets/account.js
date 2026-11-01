// /account: sign in, start a trial, see the plan, get the personal install
// line and the browser helper, and manage devices (docs/product-spec.md 3).
//
// Signing in with a new wallet goes straight to choosing a plan: there is no
// "account with nothing bought" state worth showing. Starting a trial
// charges nothing; the server reports the plan and its status.

import { config, planFor } from "./config.js";
import { errorText, formatDate, h, isMock, makeApi, mockBanner, notice, pickWallet, shortAddress, show, signIn } from "./common.js";
import { InstallBox } from "./install.js";

const PLATFORM = { linux: "Linux", windows: "Windows", macos: "macOS" };
const POLL_MS = isMock ? 400 : 3000;
const POLL_LIMIT = 60; // three minutes: Base confirms in seconds, Solana faster
const LIVE = ["trial", "active", "grace"];
const STATUS = {
  trial: (a) => `Free trial, ends ${formatDate(a.trial_ends_at)}. Then ${priceText(a)} until you cancel.`,
  active: (a) => `Renews ${formatDate(a.paid_until)} at ${priceText(a)}.`,
  grace: () => "The last payment did not go through. Top up your wallet or card within 3 days to keep hosted features.",
  lapsed: () => "Your plan has ended. RamWarden keeps running in local mode until you renew.",
  cancelled: (a) => (a.paid_until && Date.parse(a.paid_until) > Date.now() ? `Cancelled. Runs until ${formatDate(a.paid_until)}.` : "Cancelled."),
};

function priceText(a) {
  const plan = planFor(config.plans, a.tier, a.interval);
  return plan ? `$${plan.usd} a ${a.interval}` : "your plan's price";
}

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
      this.flash(notice("ok", "Card added. Your trial starts as soon as the payment provider confirms it, usually within a minute."));
    }
  }

  async signIn() {
    const area = h("div");
    const problem = h("div");
    show(
      root,
      h("h1", {}, "Your account"),
      h("p", {}, "Sign in with your wallet or your email."),
      h("p", { class: "muted" }, "Email sign-in opens soon. A new wallet starts a new account."),
      problem,
      area,
    );
    for (;;) {
      const wallet = await pickWallet(area, { purpose: "You sign a short message. It costs nothing." });
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
    const live = LIVE.includes(a.status);
    this.flashArea = h("div");
    const install = live ? h("div", { class: "install" }) : null;

    show(
      root,
      h("h1", {}, "Your account"),
      this.flashArea,
      a.tier
        ? h(
            "section",
            { class: "panel" },
            h("h2", {}, config.tiers[a.tier]?.name || "Your plan"),
            h("p", {}, (STATUS[a.status] || (() => ""))(a)),
            h("p", { class: "muted" }, "Signed in with ", a.identity?.kind === "email" ? a.identity.email : shortAddress(a.identity?.address) || "a wallet", "."),
            h("button", { class: "button secondary small", type: "button", onclick: () => { api.signOut(); location.reload(); } }, "Sign out"),
          )
        : null,
      live ? h("section", { class: "panel", id: "get" }, h("h2", {}, "Install"), h("p", {}, "Run this on the computer you want to protect."), install) : null,
      live ? this.extensionPanel() : null,
      live ? this.devicesPanel() : null,
      live ? null : this.buyPanel(),
    );
    if (install) new InstallBox(install, { api, account: a }).render();
  }

  // Browsers do not let a site add an extension on its own, so this is one
  // click to the store (or the unpacked steps until the listing is live).
  extensionPanel() {
    const action = config.chromeStore
      ? h("a", { class: "button", href: config.chromeStore, target: "_blank", rel: "noopener" }, "Add to Chrome")
      : h("p", { class: "muted" }, "Coming to the Chrome Web Store. Until then: open chrome://extensions, turn on Developer mode, choose Load unpacked, and pick the ", h("code", {}, "extension"), " folder from the ", h("a", { href: config.repo }, "source"), ". Works in Brave and Edge too.");
    return h(
      "section",
      { class: "panel", id: "extension" },
      h("h2", {}, "Add the browser helper"),
      h("p", {}, "Lets your warden unload tabs you aren't using. It pairs with RamWarden on this computer by itself."),
      action,
    );
  }

  devicesPanel() {
    const devices = this.account.devices || [];
    const limit = this.account.device_limit || config.tiers[this.account.tier]?.devices || 5;
    return h(
      "section",
      { class: "panel", id: "devices" },
      h("h2", {}, "Devices"),
      h("p", { class: "muted" }, `${devices.length} of ${limit} in use. Remove a computer to free its slot.`),
      devices.length === 0
        ? h("p", {}, "No devices yet. Run your install line to add this computer.")
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
                      if (!confirm(`Remove ${d.name || "this device"}? It drops to local mode until you sign in on it again.`)) return;
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
                  "Remove",
                ),
              ),
            ),
          ),
    );
  }

  buyPanel() {
    const any = this.methods.base || this.methods.solana || this.methods.stripe;
    const area = h("div");
    // `?plan=<id>` (sent by agents and the pricing cards) preselects a plan
    // the server sells; any other value is ignored.
    const wanted = this.plans.find((p) => String(p.id) === new URLSearchParams(location.search).get("plan"));
    let tier = wanted?.tier || "ram";
    let interval = wanted?.interval || "month";
    const plan = () => planFor(this.plans, tier, interval);
    const terms = h("p", { class: "muted" });
    const refresh = () => {
      const p = plan();
      for (const i of picker.querySelectorAll("input")) i.checked = i.value === (i.name === "tier" ? tier : interval);
      terms.textContent = p
        ? `Nothing is charged today. After ${config.trialDays} days, $${p.usd} a ${interval} until you cancel. Your wallet approves one year at most.`
        : "";
    };
    const choice = (name, value, label, sub) =>
      h(
        "label",
        { class: "plan-choice" },
        h("input", { type: "radio", name, value, onchange: () => { if (name === "tier") tier = value; else interval = value; refresh(); } }),
        h("strong", {}, label),
        sub ? h("span", {}, sub) : null,
      );
    const m = (t) => planFor(this.plans, t, "month");
    const y = (t) => planFor(this.plans, t, "year");
    const picker = h(
      "div",
      {},
      h("div", { class: "plans" }, Object.entries(config.tiers).filter(([t]) => m(t)).map(([t, info]) => choice("tier", t, info.name, `$${m(t).usd} / month`))),
      h("div", { class: "plans" }, choice("interval", "month", "Monthly"), y("pro") ? choice("interval", "year", "Annual", `$${y("ram")?.usd ?? ""} or $${y("pro").usd} / year`) : null),
    );
    const method = (key, label, sub, run) =>
      this.methods[key] ? h("button", { class: "wallet", type: "button", onclick: () => run(plan().id, area) }, h("strong", {}, label), h("span", {}, sub)) : null;
    refresh();

    return h(
      "section",
      { class: "panel", id: "buy" },
      h("h2", {}, this.account.status ? "Renew" : `Start your ${config.trialDays}-day free trial`),
      any
        ? [
            picker,
            terms,
            h(
              "div",
              { class: "wallets" },
              method("base", "USDC on Base", this.networks?.base || "MetaMask, Phantom or any EVM wallet", (p, a) => this.payCrypto("evm", p, a)),
              method("solana", "USDC on Solana", this.networks?.solana || "Phantom", (p, a) => this.payCrypto("solana", p, a)),
              method("stripe", "Card or Apple Pay", "Card, Apple Pay or Google Pay", (p, a) => this.payStripe(p, a)),
            ),
            area,
          ]
        : notice("info", "Checkout opens soon."),
    );
  }

  async payCrypto(chain, plan, area) {
    const wallet = await pickWallet(area, {
      chain,
      purpose: "Pick the wallet to pay from. It does not have to be the one you signed in with.",
    });
    try {
      show(area, notice("info", "Preparing your trial…"));
      const order = chain === "evm" ? await api.checkoutBase(plan, wallet.address) : await api.checkoutSolana(plan, wallet.address);
      show(area, notice("info", chain === "evm" ? "Confirm in your wallet: allow up to one year of the plan, then start the trial. Nothing is charged today." : "Confirm in Phantom. Nothing is charged today."));
      await wallet.pay(order);
      await this.waitForPayment(order.payment_id, area);
    } catch (err) {
      show(area, notice("error", errorText(err)));
    }
  }

  async payStripe(plan, area) {
    try {
      show(area, notice("info", "Opening card checkout…"));
      const { url } = await api.checkoutStripe(plan);
      if (isMock) {
        show(area, notice("info", "Preview mode: card checkout would open here."));
        return;
      }
      if (!/^https:\/\/checkout\.stripe\.com\//.test(url)) throw new Error("Unexpected checkout address.");
      location.assign(url);
    } catch (err) {
      show(area, notice("error", errorText(err)));
    }
  }

  async waitForPayment(id, area) {
    show(area, notice("info", "Sent. Waiting for the network to confirm it…"));
    for (let i = 0; i < POLL_LIMIT; i++) {
      await new Promise((r) => setTimeout(r, POLL_MS));
      const p = await api.payment(id);
      if (p.status === "confirmed") {
        this.account = await api.account();
        this.render();
        this.flash(notice("ok", `Your trial has started. Install RamWarden below.`));
        return;
      }
      if (p.status === "failed") {
        show(area, notice("error", p.reason || "That did not go through. Nothing was started, and a failed transaction moves no USDC."));
        return;
      }
    }
    show(area, notice("info", "Still waiting for confirmation. It is safe to close this page: your trial starts as soon as it confirms."));
  }
}

new AccountPage().start();
