// Pieces every page shares: picking the real or mock server, building DOM
// safely, and the wallet picker.
//
// All text from the server or a wallet goes into the page through
// `textContent`, never `innerHTML`, so a device name or error message can
// never become markup on a page where people sign things.

import { Api } from "./api.js";
import { config } from "./config.js";
import { EvmWallet, PhantomWallet, discoverEvmWallets, phantomProvider, walletErrorMessage } from "./wallet.js";

export const isMock = typeof location !== "undefined" && new URLSearchParams(location.search).get("mock") === "1";

export async function makeApi() {
  if (isMock) {
    const { MockApi } = await import("./mock.js");
    return new MockApi();
  }
  return new Api(config.apiBase);
}

// h("p", {class: "x"}, "text", child): a tiny element builder.
export function h(tag, attrs = {}, ...children) {
  const el = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs || {})) {
    if (v == null || v === false) continue;
    if (k.startsWith("on")) el.addEventListener(k.slice(2), v);
    else if (k === "class") el.className = v;
    else el.setAttribute(k, v === true ? "" : v);
  }
  for (const c of children.flat()) {
    if (c == null || c === false) continue;
    el.append(c instanceof Node ? c : document.createTextNode(String(c)));
  }
  return el;
}

// Keeps preview mode on when following a link between the site's pages.
export function pageHref(href) {
  if (!isMock) return href;
  const [path, hash] = href.split("#");
  return `${path}${path.includes("?") ? "&" : "?"}mock=1${hash ? "#" + hash : ""}`;
}

export function show(container, ...children) {
  container.replaceChildren(...children.flat().filter(Boolean));
}

export function notice(kind, text) {
  return h("p", { class: `notice ${kind}`, role: kind === "error" ? "alert" : "status" }, text);
}

export function shortAddress(a) {
  return a && a.length > 14 ? `${a.slice(0, 6)}…${a.slice(-4)}` : a;
}

export function formatDate(iso) {
  if (!iso) return "";
  return new Date(iso).toLocaleDateString(undefined, { year: "numeric", month: "long", day: "numeric" });
}

// Every wallet the browser offers, as buttons. Resolves with the connected
// wallet once the user picks one and approves the connection.
// `chain` limits the choice to "evm" or "solana", for paying on one network.
export async function pickWallet(container, { purpose, chain = null }) {
  const options = [];
  if (chain !== "solana") {
    for (const w of await discoverEvmWallets()) options.push({ label: w.name, sub: "Ethereum / Base", make: () => new EvmWallet(w) });
  }
  const phantom = chain !== "evm" && phantomProvider();
  if (phantom) options.push({ label: "Phantom", sub: "Solana", make: () => new PhantomWallet(phantom) });
  if (isMock) {
    const { DemoWallet } = await import("./mock.js");
    if (chain !== "solana") options.push({ label: "Demo wallet", sub: "Base (preview only)", make: () => new DemoWallet("evm") });
    if (chain !== "evm") options.push({ label: "Demo wallet", sub: "Solana (preview only)", make: () => new DemoWallet("solana") });
  }

  if (options.length === 0) {
    show(
      container,
      notice("info", "No wallet found in this browser. Install one, then reload this page."),
      h(
        "div",
        { class: "wallets" },
        h("a", { class: "button secondary", href: "https://metamask.io/download/", target: "_blank", rel: "noopener" }, "Get MetaMask"),
        h("a", { class: "button secondary", href: "https://phantom.com/download", target: "_blank", rel: "noopener" }, "Get Phantom"),
      ),
    );
    return new Promise(() => {}); // nothing to wait for until a reload
  }

  return new Promise((resolve) => {
    const status = h("div");
    const buttons = options.map((o) =>
      h(
        "button",
        {
          class: "wallet",
          type: "button",
          onclick: async () => {
            buttons.forEach((b) => (b.disabled = true));
            const wallet = o.make();
            try {
              show(status, notice("info", `Approve the connection in ${wallet.name}…`));
              await wallet.connect();
              show(status);
              resolve(wallet);
            } catch (err) {
              show(status, notice("error", walletErrorMessage(err)));
              buttons.forEach((b) => (b.disabled = false));
            }
          },
        },
        h("strong", {}, o.label),
        h("span", {}, o.sub),
      ),
    );
    show(container, h("p", { class: "muted" }, purpose), h("div", { class: "wallets" }, buttons), status);
  });
}

export function mockBanner() {
  if (!isMock) return;
  document.body.prepend(
    h("div", { class: "mock-banner" }, "Preview mode: a pretend server and demo wallets. Nothing here is real or sent anywhere."),
  );
}

// Challenge, sign, verify. The server words the message; the wallet shows it
// to the user in full before signing, and signing costs nothing.
export async function signIn(api, wallet, userCode) {
  const { message } = await api.challenge({ chain: wallet.chain, address: wallet.address, userCode });
  const signature = await wallet.sign(message);
  return api.verify({ chain: wallet.chain, address: wallet.address, message, signature });
}

// Wallet or email, one account either way (docs/product-spec.md 3.1).
// Resolves with the server's `{account, linked_device}` once one of them
// succeeds; a failure is shown in place and the visitor can try again.
export function chooseSignIn(container, api, { userCode = null, purpose = "You sign a short message. It costs nothing." } = {}) {
  return new Promise((resolve) => {
    const problem = h("div");
    const walletArea = h("div");
    const emailArea = h("div");
    show(container, problem, walletArea, h("p", { class: "or" }, "or"), emailArea);

    const wallets = async () => {
      const wallet = await pickWallet(walletArea, { purpose });
      show(walletArea, notice("info", `Check ${wallet.name}: it shows the sign-in message for ${shortAddress(wallet.address)}.`));
      try {
        resolve(await signIn(api, wallet, userCode));
      } catch (err) {
        show(problem, notice("error", errorText(err)), h("button", { class: "button secondary small", type: "button", onclick: () => { show(problem); wallets(); } }, "Try again"));
        show(walletArea);
      }
    };

    const email = () => {
      const input = h("input", { type: "email", name: "email", autocomplete: "email", placeholder: "you@example.com", "aria-label": "Email", required: true });
      const status = h("div");
      show(
        emailArea,
        h(
          "form",
          {
            class: "email-form",
            onsubmit: async (e) => {
              e.preventDefault();
              try {
                show(status, notice("info", "Sending a code…"));
                await api.emailStart({ email: input.value.trim(), userCode });
                askCode(input.value.trim());
              } catch (err) {
                show(status, notice("error", errorText(err)));
              }
            },
          },
          input,
          h("button", { class: "button secondary", type: "submit" }, "Email me a code"),
        ),
        status,
      );
    };

    const askCode = (address) => {
      const input = h("input", { name: "code", inputmode: "numeric", autocomplete: "one-time-code", maxlength: "6", placeholder: "123456", "aria-label": "Six-digit code" });
      const status = h("div");
      show(
        emailArea,
        h("p", {}, `We sent a six-digit code to ${address}.`),
        h(
          "form",
          {
            class: "email-form",
            onsubmit: async (e) => {
              e.preventDefault();
              try {
                resolve(await api.emailVerify({ email: address, code: input.value.trim() }));
              } catch (err) {
                show(status, notice("error", errorText(err)));
              }
            },
          },
          input,
          h("button", { class: "button", type: "submit" }, "Sign in"),
        ),
        h("button", { class: "linkish", type: "button", onclick: email }, "Use a different email"),
        status,
      );
      input.focus();
    };

    wallets();
    email();
  });
}

// One sentence for any failure, whether the server refused, could not be
// reached, or the wallet said no.
export function errorText(err) {
  if (err?.status === 0) return "The RamWarden server could not be reached. Hosted sign-in may not be live yet; the app works fully without it.";
  if (typeof err?.status === "number") return err.message;
  return walletErrorMessage(err);
}
