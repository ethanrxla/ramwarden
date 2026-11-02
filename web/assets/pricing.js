// The pricing block, shared by both warden tabs (docs/product-spec.md 2.7).
// Built from config.js so the two pages, and the prices on them, cannot
// drift apart. Lite sits first so Plus reads as the easy step up, and Pro
// one dollar more makes Plus look like the sensible pick.

import { annualSaving, config, planFor } from "./config.js";
import { h, pageHref } from "./common.js";

// One bullet: plain text, or {text, soon} for a part that hasn't shipped.
// A first line ending in ":" leads the list ("Everything in Lite, plus:").
function bullet(x, i) {
  const text = typeof x === "string" ? x : x.text;
  const soon = typeof x === "object" && x.soon;
  return h("li", { class: i === 0 && text.endsWith(":") ? "lead" : null }, text, soon ? h("i", { class: "soon" }, "Soon") : null);
}

export class Pricing {
  constructor(el, { plans = config.plans, tiers = config.tiers, lite = config.lite } = {}) {
    this.el = el;
    this.plans = plans;
    this.tiers = tiers;
    this.lite = lite;
    this.interval = "2mo";
  }

  render() {
    const best = Math.max(...Object.keys(this.tiers).map((t) => annualSaving(this.plans, t)));
    const toggle = h(
      "div",
      { class: "interval", role: "group", "aria-label": "Billing" },
      h("button", { type: "button", "data-interval": "2mo", onclick: () => this.set("2mo") }, "Every 2 months"),
      h("button", { type: "button", "data-interval": "year", onclick: () => this.set("year") }, "Annual", h("small", {}, `save up to ${best}%`)),
    );
    this.el.replaceChildren(
      toggle,
      h("div", { class: "price-cards" }, this.lite ? this.liteCard() : null, Object.keys(this.tiers).map((t) => this.card(t))),
      h("p", { class: "pay-methods" }, "Pay with a wallet, card or Apple Pay. Cancel any time. Lite needs no card."),
      h("p", { class: "oss-line" }, "Prefer to build it yourself? RamWarden is ", h("a", { href: config.repo }, "open source on GitHub"), "."),
    );
    this.set(this.interval);
  }

  // Lite has no plan to buy: the button signs up for a free account.
  liteCard() {
    const f = this.lite;
    return h(
      "div",
      { class: "price card tier-lite", "data-tier": "lite" },
      h("h3", {}, f.name),
      h("p", { class: "tagline" }, f.tagline),
      h("div", { class: "amount" }, "$0"),
      h("p", { class: "alt" }, "forever"),
      h("ul", {}, f.features.map((x, i) => bullet(x, i))),
      h("a", { class: "button secondary", "data-lite": "", href: pageHref("account.html") }, f.cta),
    );
  }

  card(tier) {
    const t = this.tiers[tier];
    const m = planFor(this.plans, tier, "2mo");
    const y = planFor(this.plans, tier, "year");
    return h(
      "div",
      { class: `price card tier-${tier}`, "data-tier": tier },
      t.badge ? h("span", { class: "badge" }, t.badge) : null,
      h("h3", {}, t.name),
      h("p", { class: "tagline" }, t.tagline),
      h(
        "div",
        { class: "amount" },
        h("span", { "data-price": "2mo" }, `$${m.usd}`, h("small", {}, " / 2 months")),
        h("span", { "data-price": "year" }, `$${y.usd}`, h("small", {}, " / year")),
      ),
      h(
        "p",
        { class: "alt" },
        h("span", { "data-price": "2mo" }, `or $${y.usd} / year`),
        h("span", { "data-price": "year" }, `instead of $${m.usd * 6} paid every 2 months`),
      ),
      h("ul", {}, t.features.map((x, i) => bullet(x, i))),
      h("a", { class: "button", "data-buy": tier, href: "#" }, t.trialDays ? `Start ${t.trialDays}-day free trial` : `Get ${t.name.replace(/^RamWarden /, "")}`),
      h("p", { class: "fine" }, t.trialDays ? "No charge for 14 days. Cancel any time." : "Paid from day one. Cancel any time."),
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
