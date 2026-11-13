// The corner pet as a sales and support agent (docs/product-spec.md 2.6).
//
// Clicking the pet, or its small "Ask me" button, opens a chat panel. The
// warden answers from the server's curated bank, offers a ticket when it
// can't help or the visitor asks for a person, and shows the team's reply
// the next time the panel opens. The page never decides an answer itself.
//
// Not annoying, on purpose: the panel opens only when the visitor asks, it
// never pops itself open, and while it is open the pet's jokes stay hidden.
//
// Everything from the server goes in through textContent (common.js `h`).

import { ApiError } from "./api.js";
import { h, pageHref } from "./common.js";

const store = {
  get(k) {
    try {
      return localStorage.getItem(k);
    } catch {
      return null;
    }
  },
  set(k, v) {
    try {
      if (v == null) localStorage.removeItem(k);
      else localStorage.setItem(k, v);
    } catch {
      // Private window: the chat lasts for this page only.
    }
  },
};

const NAMES = { ram: "RamWarden", threat: "ThreatWarden" };

export function errorLine(e) {
  if (e instanceof ApiError && e.offline) return "Chat is temporarily unavailable. You can still browse pricing and setup on this site.";
  return (e && e.message) || "Something went wrong. Try again in a minute.";
}

export class SupportChat {
  constructor(pet, { api, warden = "ram" }) {
    this.pet = pet;
    this.api = api;
    this.warden = warden;
    this.key = `rw.support.${warden}`;
    this.loaded = false;
    this.busy = false;
    pet.support = this; // lets the browser tests drive it
  }

  get conversation() {
    return store.get(this.key);
  }

  set conversation(v) {
    store.set(this.key, v);
  }

  start() {
    this.build();
    const sprite = this.pet.querySelector("img");
    if (sprite) {
      sprite.setAttribute("role", "button");
      sprite.setAttribute("tabindex", "0");
      sprite.setAttribute("aria-label", `Chat with ${NAMES[this.warden]}`);
      sprite.addEventListener("click", () => this.toggle());
      sprite.addEventListener("keydown", (e) => {
        if (e.key === "Enter" || e.key === " ") {
          e.preventDefault();
          this.toggle();
        }
      });
    }
    this.pet.append(
      (this.askButton = h("button", { type: "button", class: "pet-ask", "aria-expanded": "false", "aria-controls": "warden-chat", onclick: () => this.toggle() }, "Ask me")),
    );
    addEventListener("keydown", (e) => {
      if (e.key === "Escape" && !this.panel.hidden) this.close();
    });
    return this;
  }

  build() {
    this.log = h("ol", { class: "chat-log", "aria-live": "polite" });
    this.chips = h("div", { class: "chat-chips" });
    this.offers = h("div", { class: "chat-chips chat-offers" });
    this.email = h("input", { type: "email", name: "email", placeholder: "Email for the reply (optional)", autocomplete: "email", "aria-label": "Email for the reply" });
    this.ticketBox = h(
      "form",
      { class: "chat-ticket", hidden: true, onsubmit: (e) => (e.preventDefault(), this.openTicket()) },
      this.email,
      h("button", { type: "submit", class: "button" }, "Open a ticket"),
    );
    this.input = h("input", { type: "text", name: "message", maxlength: "2000", placeholder: "Ask your warden", autocomplete: "off", "aria-label": "Your question" });
    const form = h(
      "form",
      { class: "chat-form", onsubmit: (e) => (e.preventDefault(), this.send({ message: this.input.value })) },
      this.input,
      h("button", { type: "submit", class: "button" }, "Send"),
    );
    this.panel = h(
      "section",
      { class: `chat chat-${this.warden}`, id: "warden-chat", role: "dialog", "aria-label": `Chat with ${NAMES[this.warden]}`, hidden: true },
      h(
        "header",
        { class: "chat-head" },
        h("strong", {}, NAMES[this.warden]),
        h("span", { class: "chat-sub" }, "Answers about plans, setup and your account"),
        h("button", { type: "button", class: "chat-close", "aria-label": "Close the chat", onclick: () => this.close() }, "×"),
      ),
      this.log,
      this.chips,
      this.offers,
      this.ticketBox,
      form,
      h("p", { class: "chat-note" }, "Chats are kept for 30 days so the team can help. ", h("a", { href: "privacy.html" }, "Privacy")),
    );
    document.body.append(this.panel);
  }

  toggle() {
    if (this.panel.hidden) this.open();
    else this.close();
  }

  async open() {
    this.panel.hidden = false;
    this.pet.classList.add("chatting");
    this.askButton?.setAttribute("aria-expanded", "true");
    this.input.focus();
    if (!this.loaded) await this.load();
  }

  close() {
    this.panel.hidden = true;
    this.pet.classList.remove("chatting");
    this.askButton?.setAttribute("aria-expanded", "false");
    this.askButton?.focus();
  }

  // The greeting, starter chips, and the stored thread (with any reply from
  // the team) if this browser has one.
  async load() {
    this.loaded = true;
    this.log.querySelector(".chat-msg.error")?.remove();
    try {
      const r = await this.api.supportHistory({ conversation: this.conversation, warden: this.warden });
      if (!r.live) this.conversation = null;
      this.say("warden", r.greeting);
      for (const m of r.thread?.messages || []) this.say(m.from, m.text);
      this.setChips(r.chips);
      this.ticket = r.thread?.ticket || null;
    } catch (e) {
      this.loaded = false;
      this.say("warden", errorLine(e), "error");
    }
  }

  say(from, text, extra = "") {
    const who = from === "visitor" ? "You" : from === "staff" ? "RamWarden team" : NAMES[this.warden];
    this.log.append(h("li", { class: `chat-msg from-${from} ${extra}`.trim() }, h("span", { class: "chat-who" }, who), h("p", {}, text)));
    this.log.scrollTop = this.log.scrollHeight;
  }

  setChips(chips) {
    this.chips.replaceChildren(
      ...(chips || []).map((c) => h("button", { type: "button", class: "chip", onclick: () => this.send({ topic: c.topic, label: c.label }) }, c.label)),
    );
  }

  setOffer(topic) {
    const offer = topic === "trial" ? ["Start the Plus trial", "account.html?plan=1"]
      : topic === "threatwarden" ? ["See Pro", "account.html?plan=2"]
      : ["plans", "pricing", "lite"].includes(topic) ? ["Compare plans", "#pricing"] : null;
    this.offers.replaceChildren(...(offer ? [h("a", { class: "button secondary", href: pageHref(offer[1]) }, offer[0])] : []));
  }

  async send({ message, topic, label }) {
    const text = (message ?? "").trim();
    if (this.busy || (!text && !topic)) return;
    this.busy = true;
    this.say("visitor", text || label);
    this.input.value = "";
    this.setChips([]);
    const typing = h("li", { class: "chat-msg from-warden typing" }, h("p", {}, "…"));
    this.log.append(typing);
    try {
      const r = await this.api.supportChat({ conversation: this.conversation, warden: this.warden, message: text || null, topic: topic || null });
      if (r.conversation) this.conversation = r.conversation;
      typing.remove();
      this.say("warden", r.reply.text);
      this.setChips(r.reply.chips);
      this.setOffer(r.reply.topic);
      this.ticket = r.ticket || this.ticket;
      this.ticketBox.hidden = !r.reply.offer_ticket;
      // The ticket box takes room from the log; keep the answer in view.
      this.log.scrollTop = this.log.scrollHeight;
    } catch (e) {
      typing.remove();
      this.say("warden", errorLine(e), "error");
    } finally {
      this.busy = false;
    }
  }

  async openTicket() {
    if (this.busy || !this.conversation) return;
    this.busy = true;
    try {
      const r = await this.api.supportTicket({ conversation: this.conversation, email: this.email.value.trim() });
      this.ticket = r.ticket;
      this.ticketBox.hidden = true;
      this.say("warden", r.reply.text);
    } catch (e) {
      this.say("warden", errorLine(e), "error");
    } finally {
      this.busy = false;
    }
  }
}
