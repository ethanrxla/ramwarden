// Browser tests for the site, against the mock server (`?mock=1`).
// Run: node web/tests/e2e.cjs   (needs Playwright and Chromium)
//
// The wallets here are injected fakes that record what they were asked to
// do. Nothing reaches a real wallet, chain or server.

const http = require("node:http");
const fs = require("node:fs");
const path = require("node:path");
const assert = require("node:assert/strict");
const { chromium } = require("playwright");

const ROOT = path.join(__dirname, "..");
const TYPES = { ".html": "text/html", ".js": "text/javascript", ".css": "text/css", ".png": "image/png" };

function serve() {
  const server = http.createServer((req, res) => {
    const p = path.join(ROOT, decodeURIComponent(new URL(req.url, "http://x").pathname));
    if (!p.startsWith(ROOT) || !fs.existsSync(p) || fs.statSync(p).isDirectory()) {
      res.writeHead(404).end();
      return;
    }
    res.writeHead(200, { "Content-Type": TYPES[path.extname(p)] || "application/octet-stream" });
    fs.createReadStream(p).pipe(res);
  });
  return new Promise((r) => server.listen(0, "127.0.0.1", () => r(server)));
}

// An EIP-6963 MetaMask stand-in.
function fakeMetaMask() {
  window.__calls = [];
  const provider = {
    isMetaMask: true,
    async request({ method, params }) {
      window.__calls.push({ method, params });
      switch (method) {
        case "eth_requestAccounts": return ["0x8ba1f109551bd432803012645ac136ddd64dba72"];
        case "personal_sign": return "0x" + "11".repeat(65);
        case "eth_chainId": return "0x1";
        case "wallet_switchEthereumChain": return null;
        case "eth_sendTransaction": return "0x" + "22".repeat(32);
        default: throw Object.assign(new Error("unsupported " + method), { code: 4200 });
      }
    },
  };
  const info = { uuid: "test-mm", name: "MetaMask", rdns: "io.metamask", icon: "" };
  window.addEventListener("eip6963:requestProvider", () =>
    window.dispatchEvent(new CustomEvent("eip6963:announceProvider", { detail: Object.freeze({ info, provider }) })),
  );
}

function fakePhantom({ reject = false } = {}) {
  window.__calls = [];
  window.phantom = {
    solana: {
      isPhantom: true,
      async connect() { window.__calls.push({ method: "connect" }); return { publicKey: { toString: () => "7EcDhSYGxXyscszYEp35KHN8vvw3svAuLKTzXwCFLtV" } }; },
      async signMessage(bytes) {
        window.__calls.push({ method: "signMessage", text: new TextDecoder().decode(bytes) });
        if (reject) throw Object.assign(new Error("User rejected the request."), { code: 4001 });
        return { signature: new Uint8Array(64).fill(1) };
      },
      async request(r) { window.__calls.push(r); return { signature: "sig" }; },
    },
  };
}

async function run() {
  const server = await serve();
  const base = `http://127.0.0.1:${server.address().port}`;
  const browser = await chromium.launch();
  const failures = [];
  const tests = [];
  const t = (name, fn) => tests.push({ name, fn });

  async function page(init, arg) {
    const ctx = await browser.newContext();
    if (init) await ctx.addInitScript(init, arg);
    const p = await ctx.newPage();
    p.errors = [];
    p.on("pageerror", (e) => p.errors.push(e.message));
    p.on("console", (m) => m.type() === "error" && !/net::ERR|Failed to load resource|Content Security Policy/.test(m.text()) && p.errors.push(m.text()));
    return p;
  }

  t("landing page renders with no script errors and fills prices", async () => {
    const p = await page();
    await p.goto(`${base}/index.html`);
    await p.waitForSelector("#extension-action >> text=Load unpacked");
    assert.equal(await p.textContent('[data-plan="pro_30d"]'), "4");
    assert.match(await p.getAttribute('[data-download="linux"]', "href"), /github\.com\/ethanrxla\/ramwarden\/releases/);
    assert.deepEqual(p.errors, []);
  });

  t("link page: MetaMask signs the server's message for the right code", async () => {
    const p = await page(fakeMetaMask);
    await p.goto(`${base}/link.html?mock=1&code=bcdf-ghjk`);
    await p.waitForSelector("text=ethan-desktop");
    await p.getByRole("button", { name: /MetaMask/ }).click();
    await p.waitForSelector("h1:text('Device linked')");
    const calls = await p.evaluate(() => window.__calls);
    const sign = calls.find((c) => c.method === "personal_sign");
    const text = Buffer.from(sign.params[0].slice(2), "hex").toString("utf8");
    assert.match(text, /urn:ramwarden:device:BCDF-GHJK/);
    assert.equal(sign.params[1], "0x8ba1f109551bd432803012645ac136ddd64dba72");
    assert.deepEqual(p.errors, []);
  });

  t("link page: a bad or expired code asks again", async () => {
    const p = await page(fakeMetaMask);
    await p.goto(`${base}/link.html?mock=1&code=ZZZZ-ZZZZ`);
    await p.waitForSelector("text=expired");
    await p.fill("#code", "nope");
    await p.click("button[type=submit]");
    await p.waitForSelector("text=Codes are 8 letters");
    await p.fill("#code", "bcdfghjk");
    await p.click("button[type=submit]");
    await p.waitForSelector("text=ethan-desktop");
    assert.match(p.url(), /code=BCDF-GHJK/);
  });

  t("link page: a rejected signature says nothing was signed and offers a retry", async () => {
    const p = await page(fakePhantom, { reject: true });
    await p.goto(`${base}/link.html?mock=1&code=BCDF-GHJK`);
    await p.getByRole("button", { name: /Phantom/ }).click();
    await p.waitForSelector("text=Nothing was signed or sent");
    await p.waitForSelector("button:text('Try again')");
  });

  t("account page: sign in with Phantom, buy on Base with MetaMask, pass extends", async () => {
    const p = await page(() => {});
    await p.addInitScript(fakeMetaMask);
    await p.addInitScript(fakePhantom);
    await p.goto(`${base}/account.html?mock=1`);
    await p.getByRole("button", { name: /^Phantom/ }).click();
    await p.waitForSelector("h2:text('Free')");
    await p.getByRole("button", { name: /USDC on Base/ }).click();
    await p.getByRole("button", { name: /MetaMask/ }).click();
    await p.waitForSelector("text=Pro now runs until");
    const calls = await p.evaluate(() => window.__calls);
    assert.ok(calls.some((c) => c.method === "wallet_switchEthereumChain" && c.params[0].chainId === "0x14a34"), "switches to Base Sepolia");
    assert.equal(calls.filter((c) => c.method === "eth_sendTransaction").length, 2, "approve, then purchase");
    await p.waitForSelector("h2:text('RamWarden Pro')");
    assert.deepEqual(p.errors, []);
  });

  t("account page: revoking a device removes it", async () => {
    const p = await page(fakeMetaMask);
    p.on("dialog", (d) => d.accept());
    await p.goto(`${base}/account.html?mock=1`);
    await p.getByRole("button", { name: /MetaMask/ }).click();
    await p.waitForSelector("text=ethan-desktop");
    await p.getByRole("button", { name: "Revoke" }).click();
    await p.waitForSelector("text=No devices yet");
  });

  t("without the server, sign-in explains that it could not be reached", async () => {
    const p = await page(fakeMetaMask);
    await p.route("https://api.ramwarden.app/**", (r) => r.abort());
    await p.goto(`${base}/link.html?code=BCDF-GHJK`);
    await p.waitForSelector("text=could not be reached");
  });

  for (const { name, fn } of tests) {
    try {
      await fn();
      console.log(`ok   ${name}`);
    } catch (e) {
      failures.push(name);
      console.log(`FAIL ${name}\n     ${e.message.split("\n").join("\n     ")}`);
    }
  }
  await browser.close();
  server.close();
  console.log(`\n${tests.length - failures.length}/${tests.length} passed`);
  process.exit(failures.length ? 1 : 0);
}

run();
