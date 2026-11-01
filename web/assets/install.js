// The install box, shared by both warden tabs and the account page.
//
// It knows who is looking (docs/product-spec.md 2.4):
// - signed out, or signed in with no plan: OS tabs and Start free trial, no
//   command, because the command is personal;
// - trial, active or grace: a personal one-liner and installer download from
//   `POST /v1/install/link`, which signs the new computer in on first run;
// - lapsed or cancelled: Renew.
// The open-source repo sits beside it, small, as the do-it-yourself route.

import { config } from "./config.js";
import { h, pageHref } from "./common.js";

export const OS_NAMES = { macos: "macOS", windows: "Windows", linux: "Linux" };
const SHELL = { macos: "Terminal", windows: "PowerShell", linux: "Terminal" };
const NEEDS = {
  macos: "macOS on Apple silicon.",
  windows: "Windows 11, 64-bit.",
  linux: "Desktop Linux with systemd.",
};
const LIVE = ["trial", "active", "grace"];

// Best guess at the visitor's OS. A wrong guess costs one click.
export function detectOs(nav = globalThis.navigator) {
  const p = (nav?.userAgentData?.platform || nav?.platform || nav?.userAgent || "").toLowerCase();
  if (p.includes("win")) return "windows";
  if (p.includes("mac") || p.includes("darwin")) return "macos";
  if (p.includes("linux") || p.includes("x11") || p.includes("cros")) return "linux";
  return null;
}

// Which face the box shows for an account (or none).
export function installState(account) {
  if (!account || !account.status) return "signed-out";
  if (LIVE.includes(account.status)) return "ready";
  return "renew";
}

export class InstallBox {
  constructor(el, { api = null, account = null } = {}) {
    this.el = el;
    this.api = api;
    this.account = account;
    this.os = detectOs() || "macos";
    this.link = null;
  }

  async render() {
    const state = installState(this.account);
    if (state === "ready" && this.api) {
      try {
        this.link = await this.api.installLink();
      } catch {
        this.link = null; // falls back to the account page
      }
    }
    this.el.dataset.state = state;
    this.el.replaceChildren(
      h("div", { class: "install-main" }, this.tabs(), ...Object.keys(OS_NAMES).map((os) => this.panel(os, state))),
      h(
        "aside",
        { class: "install-oss" },
        h("strong", {}, "Prefer to build it yourself?"),
        h("span", {}, "RamWarden is open source. You set it up and configure everything."),
        h("a", { href: config.repo }, "View on GitHub"),
      ),
    );
    this.pick(this.os);
  }

  tabs() {
    return h(
      "div",
      { class: "os-tabs", role: "tablist", "aria-label": "Your system" },
      Object.entries(OS_NAMES).map(([os, name]) =>
        h("button", { type: "button", role: "tab", "data-os-tab": os, onclick: () => this.pick(os) }, name),
      ),
    );
  }

  panel(os, state) {
    const body = [];
    if (state === "ready" && this.link) {
      const cmd = this.link.commands?.[os];
      body.push(
        h("div", { class: "cmd-row" }, h("span", { class: "shell" }, SHELL[os]), h("code", { class: "cmd" }, cmd), this.copyButton(cmd)),
        h(
          "div",
          { class: "install-actions" },
          this.link.installers?.[os] ? h("a", { class: "button secondary small", href: this.link.installers[os] }, "Download installer") : null,
          h("span", { class: "muted" }, "Personal to you. Works once, within 30 minutes, and signs this computer in."),
        ),
      );
    } else if (state === "ready") {
      body.push(h("p", {}, NEEDS[os]), h("a", { class: "button", href: pageHref("account.html") }, "Get your install line"));
    } else if (state === "renew") {
      body.push(h("p", {}, "Your plan has ended. RamWarden keeps running in local mode until you renew."), h("a", { class: "button", href: pageHref("account.html#buy") }, "Renew"));
    } else {
      body.push(
        h("p", {}, NEEDS[os], " Your install line appears here once your trial starts."),
        h("a", { class: "button", href: "#pricing" }, `Start free trial`),
      );
    }
    return h("div", { class: "os-panel", "data-os-panel": os, role: "tabpanel" }, body);
  }

  copyButton(text) {
    const b = h("button", { class: "copy", type: "button" }, "Copy");
    b.addEventListener("click", async () => {
      try {
        await navigator.clipboard.writeText(text);
        b.textContent = "Copied";
      } catch {
        b.textContent = "Select and copy";
      }
      setTimeout(() => (b.textContent = "Copy"), 1600);
    });
    return b;
  }

  pick(os) {
    this.os = os;
    for (const t of this.el.querySelectorAll("[data-os-tab]")) t.setAttribute("aria-selected", String(t.dataset.osTab === os));
    for (const p of this.el.querySelectorAll("[data-os-panel]")) p.hidden = p.dataset.osPanel !== os;
  }
}
