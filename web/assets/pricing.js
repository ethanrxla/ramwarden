// The pricing block, shared by both warden tabs (docs/product-spec.md 2.7).
// Built from config.js so the two pages, and the prices on them, cannot
// drift apart.

import { annualSaving, config, planFor } from "./config.js";
import { h, pageHref } from "./common.js";

export class Pricing {
  constructor(el, { plans = config.plans, tiers = config.tiers, trialDays = config.trialDays } = {}) {
    this.el = el;
    this.plans = plans;
    this.tiers = tiers;
    this.trialDays = trialDays;
    this.interval = "month";
  }

  render() {
    const best = Math.max(...Object.keys(this.tiers).map((t) => annualSaving(this.plans, t)));
    const toggle = h(
      "div",
      { class: "interval", role: "group", "aria-label": "Billing" },
      h("button", { type: "button", "data-interval": "month", onclick: () => this.set("month") }, "Monthly"),
      h("button", { type: "button", "data-interval": "year", onclick: () => this.set("year") }, "Annual", h("small", {}, `save up to ${best}%`)),
    );
    this.el.replaceChildren(
      toggle,
      h("div", { class: "price-cards" }, Object.keys(this.tiers).map((t) => this.card(t))),
      h("p", { class: "pay-methods" }, "Pay with a wallet, card or Apple Pay. Cancel any time."),
      h("p", { class: "oss-line" }, "Prefer to build it yourself? RamWarden is ", h("a", { href: config.repo }, "open source on GitHub"), "."),
    );
    this.set(this.interval);
  }

  card(tier) {
    const t = this.tiers[tier];
    const m = planFor(this.plans, tier, "month");
    const y = planFor(this.plans, tier, "year");
    return h(
      "div",
      { class: `price card tier-${tier}`, "data-tier": tier },
      h("h3", {}, t.name),
      h(
        "div",
        { class: "amount" },
        h("span", { "data-price": "month" }, `$${m.usd}`, h("small", {}, " / month")),
        h("span", { "data-price": "year" }, `$${y.usd}`, h("small", {}, " / year")),
      ),
      h(
        "p",
        { class: "alt" },
        h("span", { "data-price": "month" }, `or $${y.usd} / year`),
        h("span", { "data-price": "year" }, `instead of $${m.usd * 12} paid monthly`),
      ),
      h("ul", {}, h("li", {}, `${this.trialDays}-day free trial`), h("li", {}, t.blurb), h("li", {}, `${t.devices} devices`)),
      h("a", { class: "button", "data-buy": tier, href: "#" }, "Start free trial"),
    );
  }

  set(interval) {
    this.interval = interval;
    this.el.dataset.interval = interval;
    for (const b of this.el.querySelectorAll("[data-interval]")) b.setAttribute("aria-pressed", String(b.dataset.interval === interval));
    for (const a of this.el.querySelectorAll("[data-buy]")) {
      const plan = planFor(this.plans, a.dataset.buy, interval);
      a.href = pageHref(`account.html?plan=${plan.id}`);
    }
  }
}
