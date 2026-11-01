// /link: approve a device sign-in (RFC 8628 device flow).
//
// The app opens this page as /link?code=XXXX-XXXX. The page shows which
// machine asked before anything is signed, because the one attack on a device
// flow is tricking someone into approving a code that is not theirs.

import { normaliseUserCode } from "./api.js";
import { errorText, h, makeApi, mockBanner, notice, pageHref, pickWallet, shortAddress, show, signIn } from "./common.js";

const PLATFORM = { linux: "Linux", windows: "Windows", macos: "macOS" };

const root = document.getElementById("flow");
mockBanner();
const api = await makeApi();

function askForCode(message) {
  const input = h("input", {
    id: "code",
    name: "code",
    autocomplete: "off",
    autocapitalize: "characters",
    spellcheck: "false",
    placeholder: "XXXX-XXXX",
    maxlength: "9",
    "aria-label": "Code shown in RamWarden",
  });
  const form = h(
    "form",
    {
      class: "code-form",
      onsubmit: (e) => {
        e.preventDefault();
        const code = normaliseUserCode(input.value);
        if (!code) {
          show(status, notice("error", "Codes are 8 letters, like BCDF-GHJK. Check the code in RamWarden and try again."));
          return;
        }
        history.replaceState(null, "", pageHref(`?code=${code}`));
        start(code);
      },
    },
    input,
    h("button", { class: "button", type: "submit" }, "Continue"),
  );
  const status = h("div");
  show(
    root,
    h("h1", {}, "Link a device"),
    h("p", { class: "muted" }, "Enter the code RamWarden shows after you click Sign in."),
    message,
    form,
    status,
  );
  input.focus();
}

async function start(code) {
  show(root, h("h1", {}, "Link a device"), notice("info", "Checking the code…"));
  let device;
  try {
    device = await api.lookupDevice(code);
  } catch (err) {
    askForCode(notice("error", errorText(err)));
    return;
  }

  const walletArea = h("div");
  show(
    root,
    h("h1", {}, "Link a device"),
    h(
      "div",
      { class: "device-card" },
      h("span", { class: "code" }, code),
      h("div", {}, h("strong", {}, device.device_name || "A RamWarden install"), h("span", { class: "muted" }, PLATFORM[device.platform] || device.platform || "")),
    ),
    h("p", {}, "Only continue if this is the code your own RamWarden is showing right now. If someone sent you this link, close the page."),
    walletArea,
  );

  const wallet = await pickWallet(walletArea, {
    purpose: "Sign in with a wallet. You will sign a short message; it costs no gas and moves no funds.",
  });

  show(walletArea, notice("info", `Check ${wallet.name}: it shows the sign-in message for ${shortAddress(wallet.address)}.`));
  try {
    await signIn(api, wallet, code);
  } catch (err) {
    show(walletArea, notice("error", errorText(err)), h("button", { class: "button secondary", type: "button", onclick: () => start(code) }, "Try again"));
    return;
  }

  show(
    root,
    h("h1", {}, "Device linked"),
    notice("ok", `${device.device_name || "Your device"} is now signed in to your RamWarden account.`),
    h("p", {}, "Go back to RamWarden. It finishes signing in by itself within a few seconds, and you can close this tab."),
    h("p", {}, h("a", { href: pageHref("account.html") }, "Manage your account and devices")),
  );
}

const code = normaliseUserCode(new URLSearchParams(location.search).get("code"));
if (code) start(code);
else askForCode(null);
