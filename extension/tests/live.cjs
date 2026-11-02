// Isolated, disposable Chromium profile. No normal browser profile is opened.
// Requires Playwright; CHROME_PATH can select an installed Chrome executable.
// The worker clock advances to test inactivity without waiting 24 hours.
const { chromium } = require('playwright');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const assert = require('node:assert/strict');

(async () => {
  const profile = fs.mkdtempSync(path.join(os.tmpdir(), 'rw-live-'));
  // Copy only this test extension and disable its real-daemon connection before startup.
  const extension = path.join(profile, 'extension');
  fs.cpSync(path.resolve(__dirname, '..'), extension, {recursive:true});
  const background = path.join(extension, 'background.js');
  fs.writeFileSync(background, fs.readFileSync(background, 'utf8').replace('const DEFAULT_HOST = "localhost:7823";', 'const DEFAULT_HOST = "127.0.0.1:1";'));
  const context = await chromium.launchPersistentContext(profile, {
    executablePath: process.env.CHROME_PATH || undefined,
    headless: true, ignoreDefaultArgs: ['--disable-extensions'], args: ['--enable-unsafe-extension-debugging'],
  });
  try {
    const cdp = await context.browser().newBrowserCDPSession();
    await cdp.send('Extensions.loadUnpacked', { path: extension });
    const worker = context.serviceWorkers()[0] || await context.waitForEvent('serviceworker');
    const evaluate = worker.evaluate.bind(worker);
    worker.evaluate = async (...args) => {
      let timer;
      try { return await Promise.race([evaluate(...args), new Promise((_, reject) => {
        timer = setTimeout(() => reject(new Error('Extension worker timed out: ' + String(args[0]).slice(0,160))), 15000);
      })]); } finally { clearTimeout(timer); }
    };
    await worker.evaluate(async () => { await safetyReady; _httpUrl = 'http://127.0.0.1:1'; if (typeof ws !== 'undefined' && ws) ws.close(); });
    await context.route('https://fixture.example.org/**', route => route.fulfill({ contentType: 'text/html', body: '<title>Disposable fixture</title><input aria-label="Work"><p>Test only</p>' }));
    const make = async (suffix) => { const p = await context.newPage(); await p.goto('https://fixture.example.org/' + suffix); return p; };
    const edited = await make('login');
    await edited.locator('input').fill('synthetic test');
    const cart = await make('cart');
    const candidate = await make('article');
    const guard = await make('guard');
    await guard.bringToFront();
    await worker.evaluate(() => new Promise(resolve => setTimeout(resolve, 100)));
    const tabs = await worker.evaluate(() => chrome.tabs.query({}));
    const id = suffix => tabs.find(t => t.url.endsWith('/' + suffix)).id;
    assert.equal(await worker.evaluate(i => safety.work[i]?.dirty, id('login')), true, 'real input detected without its contents');
    await worker.evaluate(() => { globalThis.realNow = Date.now; Date.now = () => realNow() + 25 * 3600000; });
    const close = async (ids, override = {}) => worker.evaluate(async ({ids, override}) => {
      const tabs = await chrome.tabs.query({});
      return closeSafe({requestId:'live-test',expiresAt:Date.now()+10000,minInactiveMinutes:1440,
        tabs:ids.map(id=>({id,url:tabs.find(t=>t.id===id)?.url})),...override});
    }, {ids,override});
    assert.deepEqual(await close([id('cart')]), [], 'cart stays open even after inactivity');
    assert.deepEqual(await close([id('login')]), [], 'work in progress stays open');
    assert.deepEqual(await close([id('guard')]), [], 'active tab stays open');
    assert.deepEqual(await close([id('article')], {expiresAt:0}), [], 'expired command does nothing');
    assert.deepEqual(await close([id('article')], {tabs:[{id:id('article'),url:'https://fixture.example.org/wrong'}]}), [], 'identity mismatch stays open');
    assert.deepEqual(await close([id('article')]), [id('article')], 'only selected eligible fixture closes');
    const reopened = await make('article');
    await guard.bringToFront();
    // A report observes the final URL after navigation, as in the daemon flow.
    await worker.evaluate(async () => { for (const t of await chrome.tabs.query({})) observeReopen(t); });
    assert.ok(await worker.evaluate(() => safety.pages['https://fixture.example.org/article']?.until > Date.now()), 'reopened URL protected across tab IDs');
    const reopenedId = await worker.evaluate(async () => (await chrome.tabs.query({})).find(t=>t.url.endsWith('/article')).id);
    assert.deepEqual(await close([reopenedId]), [], 'reopened tab refuses closing');
    await worker.evaluate(async i => keepTabs([{id:i,url:'https://fixture.example.org/login'}]), id('login'));
    const saved = await worker.evaluate(() => chrome.storage.local.get('tabSafety'));
    assert.ok(saved.tabSafety.pages['https://fixture.example.org/login'], 'Keep persisted locally');
    await reopened.close();
    await worker.evaluate(() => new Promise(resolve => setTimeout(resolve,100)));
    assert.equal(await worker.evaluate(() => safety.pages['https://fixture.example.org/article']), undefined, 'manual close clears protection');
    assert.equal(edited.isClosed(), false);
    console.log('PASS: real activity, active/work protections, expiry, identity, selected close, reopen, Keep persistence, manual close');
  } finally {
    await context.close();
    // Preserve the disposable profile for diagnostics; never access user profiles.
    console.log('Disposable profile:', profile);
  }
})().catch(error => { console.error(error); process.exitCode = 1; });


