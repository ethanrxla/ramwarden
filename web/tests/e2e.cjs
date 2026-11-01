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
const TYPES = { ".html": "text/html", ".js": "text/javascript", ".css": "text/css", ".png": "image/png", ".svg": "image/svg+xml", ".json": "application/json", ".mp4": "video/mp4", ".jpg": "image/jpeg" };

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

  t("RamWarden tab: install box, pricing, scroll world and pet", async () => {
    const p = await page();
    await p.goto(`${base}/index.html`);
    // Signed out: one panel (Linux in CI), a trial button and no command.
    await p.waitForSelector('[data-install][data-state="signed-out"]');
    assert.deepEqual(await p.$$eval("[data-os-panel]", (ps) => ps.filter((x) => x.offsetParent !== null).map((x) => x.dataset.osPanel)), ["linux"]);
    assert.equal(await p.$$eval("[data-install] .cmd", (c) => c.length), 0);
    assert.ok(await p.isVisible('[data-os-panel="linux"] >> text=Choose a plan'));
    // Pricing: monthly by default, the toggle switches prices and plan links.
    assert.equal(await p.textContent('.tier-ram [data-price="month"]'), "$4 / month");
    assert.equal(await p.textContent('.tier-pro [data-price="month"]'), "$5 / month");
    assert.match(await p.getAttribute('[data-buy="pro"]', "href"), /plan=2$/);
    // Three cards: Free signs up, Plus has the trial, Pro has none.
    assert.equal(await p.$$eval(".price-cards .price", (c) => c.map((x) => x.dataset.tier).join()), "lite,ram,pro");
    assert.equal((await p.textContent(".tier-lite h3")).trim(), "RamWarden Lite");
    assert.equal((await p.textContent('[data-buy="ram"]')).trim(), "Start 14-day free trial");
    assert.equal((await p.textContent('[data-buy="pro"]')).trim(), "Get Pro");
    await p.click('[data-interval="year"]');
    assert.ok(await p.isVisible('.tier-pro >> text=$24'));
    assert.match(await p.getAttribute('[data-buy="ram"]', "href"), /plan=3$/);
    assert.ok(await p.isVisible("text=open source on GitHub"));
    // The hero is the radar with RamWarden's own ram; the demo is in How it works.
    assert.ok(await p.isVisible('#top .radar img[data-sprite="ram"]'));
    assert.ok(await p.isVisible("#how [data-memory-lab]"));
    // The film is scrubbed by scroll.
    await p.waitForFunction(() => document.querySelector("[data-scrub]").classList.contains("live"));
    await p.evaluate(() => { const w = document.querySelector("[data-scrub]"); scrollTo(0, w.offsetTop + (w.offsetHeight - innerHeight) * 0.6); });
    await p.waitForFunction(() => document.querySelector(".world-video").currentTime > 2);
    assert.ok(await p.isVisible(".world .beat.on .world-line"), "second beat shows mid-film");
    // The pet speaks for the section in view, jokes on cue, and mutes.
    await p.evaluate(() => document.getElementById("pricing").scrollIntoView());
    await p.waitForFunction(() => /Free to start/.test(document.querySelector("[data-say]").textContent));
    assert.equal(await p.evaluate(() => document.querySelector("[data-pet]").warden.joke()), true);
    await p.click("[data-mute]");
    assert.equal(await p.isVisible("[data-say]"), false);
    assert.equal(await p.evaluate(() => document.querySelector("[data-pet]").warden.joke()), false);
    // Look: the app's controls are all there; a swatch and the pupil picker
    // redraw every ram sprite, and the look is remembered.
    for (const part of ["wool", "horns", "hooves", "eyes", "pupil"]) assert.ok(await p.isVisible(`#meet [data-part="${part}"]`), part);
    assert.ok(await p.isVisible("#meet [data-reset-eyes]"));
    await p.click('.swatch[data-wool="#3fa2f2"]');
    await p.selectOption('#meet [data-part="pupil"]', "round");
    await p.waitForFunction(() => [...document.querySelectorAll('img[data-sprite="ram"]')].every((i) => i.src.startsWith("data:image/svg+xml") && i.src.includes("3fa2f2")));
    assert.equal(await p.inputValue('#meet [data-part="wool"]'), "#3fa2f2");
    await p.reload();
    await p.waitForFunction(() => document.querySelector('[data-pet] img').src.includes("3fa2f2"));
    assert.equal(await p.inputValue('#meet [data-part="pupil"]'), "round");
    assert.equal(await p.isVisible("[data-say]"), false, "mute is remembered");
    assert.deepEqual(p.errors, []);
  });

  t("ThreatWarden tab: same skeleton, own theme, green clip scrubbed by scroll", async () => {
    const p = await page();
    await p.goto(`${base}/threat.html`);
    assert.equal(await p.getAttribute('.warden-tabs a[aria-current="page"]', "href"), "threat.html");
    const bg = await p.evaluate(() => getComputedStyle(document.body).getPropertyValue("--bg").trim());
    assert.equal(bg, "#050407");
    assert.match(await p.textContent(".catchline em"), /click every link/);
    assert.ok(await p.isVisible('[data-pet] img[src$="threat.svg"]'));
    await p.waitForSelector('[data-install][data-state="signed-out"]');
    assert.equal(await p.textContent('.tier-pro [data-price="month"]'), "$5 / month");
    assert.ok(await p.isVisible('#top .radar img[data-sprite="threat"]'), "the radar stays in the hero");
    assert.ok(await p.isVisible('#meet [data-part="visor"]'));
    assert.ok(await p.isVisible('.tier-pro >> text=Paid from day one'));
    await p.waitForFunction(() => document.querySelector("[data-scrub]").classList.contains("live"));
    await p.evaluate(() => { const w = document.querySelector("[data-scrub]"); scrollTo(0, w.offsetTop + (w.offsetHeight - innerHeight) * 0.8); });
    await p.waitForFunction(() => document.querySelector(".world-video").currentTime > 3);
    assert.deepEqual(p.errors, []);
  });

  t("install box: personal line when signed in, Renew when lapsed", async () => {
    const p = await page();
    await p.goto(`${base}/index.html?mock=1&as=trial`);
    await p.waitForSelector('[data-install][data-state="ready"]');
    assert.match(await p.textContent('[data-os-panel="linux"] .cmd'), /^curl -fsSL https:\/\/get\.ramwarden\.app\/i\/\S+ \| sh$/);
    assert.match(await p.textContent('[data-os-panel="windows"] .cmd'), /^irm https:.* \| iex$/);
    assert.ok(await p.isVisible('[data-os-panel="linux"] >> text=Download installer'));
    await p.goto(`${base}/index.html?mock=1&as=lapsed`);
    await p.waitForSelector('[data-install][data-state="renew"]');
    assert.ok(await p.isVisible('[data-os-panel="linux"] >> text=Renew'));
    assert.deepEqual(p.errors, []);
  });

  t("Lite gets its install line and Upgrade to Plus; Plus offers Upgrade to Pro", async () => {
    const p = await page();
    await p.goto(`${base}/index.html?mock=1&as=lite`);
    await p.waitForSelector('[data-install][data-state="lite"] [data-os-panel="linux"] .cmd');
    await p.goto(`${base}/account.html?mock=1&as=lite`);
    await p.waitForSelector("h2:text('RamWarden Lite')");
    await p.waitForSelector(".os-panel:not([hidden]) >> text=Download installer");
    assert.ok(await p.isVisible("h2:text('Upgrade from Lite')"));
    assert.equal((await p.textContent("[data-upgrade]")).trim(), "Upgrade to Plus");
    await p.goto(`${base}/account.html?mock=1&as=trial`);
    await p.waitForSelector("h2:text('RamWarden Plus')");
    assert.equal(await p.isVisible("#buy"), false, "no plan picker until asked");
    await p.click('[data-upgrade="pro"]');
    await p.waitForSelector("h2:text('Upgrade to Pro')");
    assert.equal(await p.isChecked('input[name=tier][value="pro"]'), true);
    assert.match(await p.textContent("#buy p.muted"), /charged today/);
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

  t("account page: sign in with Phantom, start a trial on Base with MetaMask, get the install line", async () => {
    const p = await page(() => {});
    await p.addInitScript(fakeMetaMask);
    await p.addInitScript(fakePhantom);
    await p.goto(`${base}/account.html?mock=1`);
    await p.getByRole("button", { name: /^Phantom/ }).click();
    await p.waitForSelector("h2:text('Plus is free for 14 days')");
    assert.ok(await p.isVisible("text=Nothing is charged today"));
    await p.getByRole("button", { name: /USDC on Base/ }).click();
    await p.getByRole("button", { name: /MetaMask/ }).click();
    await p.waitForSelector("text=Your trial has started");
    const calls = await p.evaluate(() => window.__calls);
    assert.ok(calls.some((c) => c.method === "wallet_switchEthereumChain" && c.params[0].chainId === "0x14a34"), "switches to Base Sepolia");
    assert.equal(calls.filter((c) => c.method === "eth_sendTransaction").length, 2, "capped approve, then startTrial");
    await p.waitForSelector("h2:text('RamWarden')");
    await p.waitForSelector('[data-os-panel="linux"] .cmd');
    assert.ok(await p.isVisible("h2:text('Add the browser helper')"));
    assert.deepEqual(p.errors, []);
  });

  t("account page: ?plan= preselects a plan the server sells, ignores others", async () => {
    for (const [q, tier, interval] of [["4", "pro", "year"], ["2", "pro", "month"], ["99", "ram", "month"], ["", "ram", "month"]]) {
      const p = await page(fakeMetaMask);
      await p.goto(`${base}/account.html?mock=1&plan=${q}`);
      await p.getByRole("button", { name: /MetaMask/ }).click();
      await p.waitForSelector("input[name=tier]");
      assert.equal(await p.$eval("input[name=tier]:checked", (i) => i.value), tier, `plan=${q}`);
      assert.equal(await p.$eval("input[name=interval]:checked", (i) => i.value), interval, `plan=${q}`);
    }
  });

  t("account page: email sign-in with a code, then cancel the card trial", async () => {
    const p = await page();
    p.on("dialog", (d) => d.accept());
    await p.goto(`${base}/account.html?mock=1`);
    await p.fill("input[name=email]", "ethan@example.com");
    await p.click("text=Email me a code");
    await p.fill("input[name=code]", "000000");
    await p.click("button:text('Sign in')");
    await p.waitForSelector("text=That code is wrong");
    await p.fill("input[name=code]", "123456");
    await p.click("button:text('Sign in')");
    await p.waitForSelector("h2:text('Plus is free for 14 days')");
    await p.getByRole("button", { name: /Card or Apple Pay/ }).click();
    await p.waitForSelector("text=Your trial has started");
    await p.click("button:text('Cancel plan')");
    await p.waitForSelector("text=Cancelled. Nothing more will be charged.");
    assert.deepEqual(p.errors, []);
  });

  t("account page: removing a device frees its slot", async () => {
    const p = await page();
    p.on("dialog", (d) => d.accept());
    await p.goto(`${base}/account.html?mock=1&as=active`);
    await p.waitForSelector("text=ethan-desktop");
    assert.ok(await p.isVisible("text=1 of 5 in use"));
    await p.getByRole("button", { name: "Remove" }).click();
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
