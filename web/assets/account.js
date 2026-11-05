// /account: sign in, start a trial, see the plan, get the personal install
// line and the browser helper, and manage devices (docs/product-spec.md 3).
//
// A signed-in account with nothing bought is RamWarden Lite: free, rules
// only, 1 device, with its install line and an Upgrade to Plus. Plus shows
// Upgrade to Pro. The server enforces every limit; this page only offers the
// step up. Starting the Plus trial charges nothing; Pro is charged at once.

import { PER_YEAR, PERIOD, config, planFor } from "./config.js";
import { publishAccount, chooseSignIn, errorText, formatDate, h, isMock, makeApi, mockBanner, notice, pickWallet, shortAddress, show } from "./common.js";
import { InstallBox, installState } from "./install.js";

const PLATFORM = { linux: "Linux", windows: "Windows", macos: "macOS" };
const POLL_MS = isMock ? 400 : 3000;
const POLL_LIMIT = 60; // three minutes: Base confirms in seconds, Solana faster
const STATUS = {
  trial: (a) => `Free trial, ends ${formatDate(a.trial_ends_at)}. Then ${priceText(a)} until you cancel.`,
  active: (a) => `Renews ${formatDate(a.paid_until)} at ${priceText(a)}.`,
  grace: () => "The last payment did not go through. Top up your wallet or card within 3 days to keep hosted features.",
  lapsed: () => "Your plan has ended. RamWarden keeps running in local mode until you renew.",
  cancelled: (a) => (a.paid_until && Date.parse(a.paid_until) > Date.now() ? `Cancelled. Runs until ${formatDate(a.paid_until)}.` : "Cancelled."),
};

function priceText(a) {
  const plan = planFor(config.plans, a.tier, a.interval);
  return plan ? `$${plan.usd} every ${PERIOD[a.interval] || a.interval}` : "your plan's price";
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
    // The session is an HttpOnly cookie, so the only way to know is to ask.
    try {
      this.account = await api.account();
    } catch (err) {
      if (err.status !== 401 && !err.offline) {
        show(root, h("h1", {}, "Your account"), notice("error", errorText(err)));
        return;
      }
    }
    const emailOnly = new URLSearchParams(location.search).get("signin") === "email";
    if (!this.account || emailOnly) await this.signIn(emailOnly);
    if (emailOnly) {
      const url = new URL(location.href); url.searchParams.delete("signin");
      history.replaceState(null, "", url);
    }
    await this.loadCheckout();
    this.render();
    // Back from card checkout: follow the payment until it confirms.
    const q = new URLSearchParams(location.search);
    if (q.get("checkout") === "done" && q.get("payment")) {
      const area = h("div");
      this.flashArea.replaceChildren(area);
      this.waitForPayment(q.get("payment"), area);
    }
  }

  async signIn(emailOnly = false) {
    const area = h("div");
    show(root, h("h1", {}, emailOnly ? "Email sign-in" : "Your account"),
      h("p", {}, emailOnly ? "Enter your email, then request a code or use one you already received. Signing in switches this browser to the account for that email." : "Sign in with your wallet or your email."), area);
    this.account = (await chooseSignIn(area, api, { emailOnly })).account;
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
    publishAccount(a);
    const state = installState(a);
    const live = state === "ready" || state === "lite";
    // What the Upgrade button offers: Lite and lapsed go to Plus, Plus to Pro.
    const next = state === "lite" ? "ram" : state === "ready" && a.tier === "ram" ? "pro" : null;
    this.flashArea = h("div");
    const install = live ? h("div", { class: "install" }) : null;

    show(
      root,
      h("h1", {}, "Your account"),
      this.flashArea,
      a.identity || a.tier
        ? h(
            "section",
            { class: "panel" },
            h("h2", {}, a.tier ? config.tiers[a.tier]?.name || "Your plan" : config.lite.name),
            h("p", {}, a.developer_access ? "Developer access · complimentary. No card or wallet will be charged." : a.status ? (STATUS[a.status] || (() => ""))(a) : "Free. Your warden runs our enhanced memory algorithm on 1 computer. Plus adds the AI companion."),
            h("p", { class: "muted" }, "Account: ", a.identity?.kind === "github" ? `GitHub account ${a.identity.id}` : a.identity?.kind === "email" ? a.identity.email : shortAddress(a.identity?.address) || "a wallet", "."),
            h(
              "div",
              { class: "row-actions" },
              h("button", { class: "button secondary small", type: "button", onclick: async () => { await api.logout().catch(() => {}); location.reload(); } }, "Sign out"),
              h("a", { class: "button secondary small", href: "account.html?signin=email" }, "Sign in with email instead"),
              next && !a.developer_access ? h("a", { class: "button small", "data-upgrade": next, href: "#buy", onclick: () => this.upgradeTo(next) }, next === "pro" ? "Upgrade to Pro" : "Upgrade to Plus") : null,
              !a.developer_access && ["trial", "active", "grace"].includes(a.status) ? h("button", { class: "button secondary small", type: "button", onclick: (e) => this.cancel(e.target) }, "Cancel plan") : null,
            ),
          )
        : null,
      live ? h("section", { class: "panel", id: "get" }, h("h2", {}, "Install"), h("p", {}, "Run this on the computer you want to protect."), install) : null,
      live ? this.extensionPanel() : null,
      this.signInMethods(),
      this.usagePanel(),
      live ? this.devicesPanel() : null,
      state === "ready" && !this.upgrading ? null : this.buyPanel(),
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

  usagePanel() {
    const status=h("div");
    const input=h("input",{type:"checkbox","aria-label":"Share usage diagnostics"});
    input.checked=this.account.prefs?.share_usage === true;
    input.addEventListener("change",async () => {
      input.disabled=true;
      try {
        const result=await api.savePrefs({...this.account.prefs,share_usage:input.checked});
        this.account.prefs=result.prefs;
        show(status,notice("ok",input.checked ? "Sharing starts now on connected devices." : "Usage sharing is off."));
      } catch(error) {input.checked=!input.checked;show(status,notice("error",errorText(error)));}
      finally {input.disabled=false;}
    });
    return h("section",{class:"panel",id:"usage-sharing"},h("h2",{},"Help improve your warden"),
      h("label",{},input," Share usage diagnostics"),
      h("p",{class:"muted"},"Share daily action counts and measured RAM recovery, including close, reopen, pause and resume events. No tab titles, URLs, page contents, app names or file paths. Summaries are kept for 90 days."),
      h("p",{class:"muted"},"Off by default. Only connected devices can send data, starting when you enable it."),status);
  }

  signInMethods() {
    const identities = this.account.identities || [this.account.identity].filter(Boolean);
    const github = h("span");
    if (api.githubConfig) api.githubConfig().then(c => {if(c.enabled) github.append(h("button",{class:"button secondary small",onclick:()=>this.linkMethod("github")},"Add GitHub"));}).catch(()=>{});
    return h("section", {class:"panel",id:"sign-in-methods"},
      h("h2", {}, "Sign-in methods"),
      h("p", {}, "Connect sign-in methods to this same account and subscription."),
      h("ul", {}, identities.map(i => h("li", {}, i.kind === "github" ? `GitHub account ${i.id}` : i.kind === "email" ? i.email : `${i.chain === "solana" ? "Solana" : "Ethereum / Base"}: ${shortAddress(i.address)}`))),
      h("div", {class:"row-actions"},
        h("button", {class:"button secondary small",onclick:() => this.linkMethod("wallet")}, "Add a wallet"),
        h("button", {class:"button secondary small",onclick:() => this.linkMethod("email")}, "Add an email"),github));
  }

  async linkMethod(kind) {
    const area = h("div");
    show(root, h("h1", {}, kind === "github" ? "Add GitHub" : kind === "email" ? "Add an email" : "Add a wallet"),
      h("p", {}, "Verify the new sign-in method to add it to this account. Your plan and devices stay here; this does not start a payment."),
      area, h("button", {class:"linkish",onclick:() => this.render()}, "Back to account"));
    this.account = (await chooseSignIn(area, api, {link:true,emailOnly:kind === "email",walletOnly:kind === "wallet",githubOnly:kind === "github",purpose:"Verify this wallet to add it to your account. No payment."})).account;
    this.render();
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

  // Upgrade opens the plan picker on the next tier. Plus to Pro is an
  // upgrade of the running plan, which the server prices.
  upgradeTo(tier) {
    this.upgrading = tier;
    this.render();
    document.getElementById("buy")?.scrollIntoView();
  }

  buyPanel() {
    const any = this.methods.base || this.methods.solana || this.methods.stripe;
    const area = h("div");
    // `?plan=<id>` (sent by agents and the pricing cards) preselects a plan
    // the server sells; any other value is ignored.
    const wanted = this.plans.find((p) => String(p.id) === new URLSearchParams(location.search).get("plan"));
    let tier = this.upgrading || wanted?.tier || "ram";
    let interval = wanted?.interval || "month";
    const plan = () => planFor(this.plans, tier, interval);
    const terms = h("p", { class: "muted" });
    const refresh = () => {
      const p = plan();
      for (const i of picker.querySelectorAll("input")) i.checked = i.value === (i.name === "tier" ? tier : interval);
      const approve = p?.approval_usd ?? (p && p.usd * (PER_YEAR[p.interval] || 1));
      const per = `every ${PERIOD[interval] || interval}`;
      // Only Plus has a trial; Pro is paid from day one.
      const trialDays = p ? (p.trial_days ?? config.tiers[p.tier]?.trialDays ?? 0) : 0;
      terms.textContent = !p
        ? ""
        : this.account.trial_available === false || !trialDays
          ? `$${p.usd} ${per}, charged today, until you cancel. A wallet approves $${approve} at most (one year).`
          : `Nothing is charged today. After ${trialDays} days, $${p.usd} ${per} until you cancel. A wallet approves $${approve} at most (one year).`;
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
      h(
        "h2",
        {},
        this.upgrading === "pro"
          ? "Upgrade to Pro"
          : this.account.status
            ? "Renew"
            : this.account.trial_available === false
              ? "Upgrade from Lite"
              : "Upgrade from Lite. Plus is free for 14 days.",
      ),
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
      show(area, notice("info", this.upgrading ? "Preparing your upgrade…" : "Preparing your plan…"));
      const order = chain === "evm" ? await api.checkoutBase(plan, wallet.address) : await api.checkoutSolana(plan, wallet.address);
      let payer = wallet;
      // An upgrade changes the running plan, so it has to be sent from the
      // wallet that already pays for it.
      if (order.upgrade && order.payer && order.payer.toLowerCase() !== String(wallet.address).toLowerCase()) {
        payer = await pickWallet(area, { chain, purpose: `Upgrades are confirmed in the wallet that pays (${shortAddress(order.payer)}).` });
      }
      const say = order.upgrade
        ? "Confirm the upgrade in your wallet. Pro is charged from today."
        : order.trial
          ? chain === "evm"
            ? "Confirm in your wallet: allow up to one year of the plan, then start the trial. Nothing is charged today."
            : "Confirm in Phantom. Nothing is charged today."
          : "Confirm in your wallet. The first payment is charged today.";
      show(area, notice("info", say));
      await payer.pay(order);
      await this.waitForPayment(order.payment_id, area);
    } catch (err) {
      show(area, notice("error", errorText(err)));
    }
  }

  async payStripe(plan, area) {
    try {
      show(area, notice("info", "Opening card checkout…"));
      const { url, payment_id, upgrade } = await api.checkoutStripe(plan);
      // A card upgrade changes the subscription in place, prorated: there is
      // no checkout page, only the confirmation to follow.
      if (upgrade) {
        show(area, notice("info", "Upgrading your card plan…"));
        await this.waitForPayment(payment_id, area);
        return;
      }
      if (isMock) {
        show(area, notice("info", "Preview mode: card checkout would open here."));
        await this.waitForPayment(payment_id, area);
        return;
      }
      if (!/^https:\/\/checkout\.stripe\.com\//.test(url)) throw new Error("Unexpected checkout address.");
      location.assign(url);
    } catch (err) {
      show(area, notice("error", errorText(err)));
    }
  }

  // Card plans cancel server side. Wallet plans are cancelled from the wallet
  // that pays, so the server hands back that transaction to send.
  async cancel(button) {
    if (!confirm("Cancel your plan? It keeps working until the end of what you've paid for, then RamWarden drops to Lite.")) return;
    button.disabled = true;
    try {
      const r = await api.cancel();
      if (r.transactions) {
        const area = h("div");
        this.flash(area);
        const wallet = await pickWallet(area, { chain: "evm", purpose: `Confirm the cancel in the wallet that pays (${shortAddress(r.payer)}). It costs a little gas and moves no USDC.` });
        await wallet.pay(r);
      }
      this.account = await api.account();
      this.render();
      this.flash(notice("ok", "Cancelled. Nothing more will be charged."));
    } catch (err) {
      button.disabled = false;
      this.flash(notice("error", errorText(err)));
    }
  }

  async waitForPayment(id, area) {
    show(area, notice("info", "Sent. Waiting for the network to confirm it…"));
    for (let i = 0; i < POLL_LIMIT; i++) {
      await new Promise((r) => setTimeout(r, POLL_MS));
      const p = await api.payment(id);
      if (p.status === "confirmed") {
        this.account = await api.account();
        this.upgrading = null;
        this.render();
        this.flash(notice("ok", this.account.status === "trial" ? "Your trial has started. Install RamWarden below." : "You're all set. Install RamWarden below."));
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
