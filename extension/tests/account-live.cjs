// Real extension popup, disposable browser profile, mocked daemon/account API.
const { chromium } = require('playwright');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const assert = require('node:assert/strict');
(async () => {
  const profile = fs.mkdtempSync(path.join(os.tmpdir(), 'rw-account-ui-'));
  const extension = path.join(profile, 'extension');
  fs.cpSync(path.resolve(__dirname, '..'), extension, {recursive:true});
  const bg = path.join(extension, 'background.js');
  fs.writeFileSync(bg, fs.readFileSync(bg,'utf8').replace('const DEFAULT_HOST = "localhost:7823";', 'const DEFAULT_HOST = "127.0.0.1:1";'));
  const context = await chromium.launchPersistentContext(profile, {
    channel: 'chromium', executablePath:process.env.CHROME_PATH || undefined, headless:true,
    ignoreDefaultArgs:['--disable-extensions'], args:['--enable-unsafe-extension-debugging'],
  });
  try {
    let account = {status:'signed_out'}, starts = 0, disconnects = 0;
    await context.route('http://localhost:7823/**', async route => {
      const p = new URL(route.request().url()).pathname;
      if (p === '/account/start') {
        starts++; account = {status:'pending',user_code:'TEST-ONLY',verification_uri_complete:'https://fixture.example.org/link.html?code=TEST-ONLY'};
      }
      if (p === '/account/disconnect') { disconnects++; account = {status:'signed_out'}; }
      const body = p.startsWith('/account') ? account : p === '/stats' ? {percent:25,used_mb:1024,total_mb:4096} : {};
      await route.fulfill({json:body});
    });
    await context.route('https://fixture.example.org/**', route => route.fulfill({body:'Disposable sign-in approval fixture'}));
    const cdp = await context.browser().newBrowserCDPSession();
    const {id} = await cdp.send('Extensions.loadUnpacked', {path:extension});
    const page = await context.newPage();
    await page.goto(`chrome-extension://${id}/popup.html`);
    await page.locator('#account-signin').waitFor({state:'visible'});
    const opening = context.waitForEvent('page');
    await page.locator('#account-signin').click();
    const approval = await opening;
    await approval.waitForURL('**/link.html?code=TEST-ONLY');
    assert.equal(starts,1);
    assert.match(await page.locator('#account-status').innerText(),/TEST-ONLY/);
    // Daemon completes the approval while the popup is closed.
    await page.close();
    account = {status:'linked',verified:true,tier:'pro',threat_unlocked:true};
    const reopened = await context.newPage(); await reopened.goto(`chrome-extension://${id}/popup.html`);
    await reopened.locator('#account-refresh').waitFor({state:'visible'});
    assert.equal(await reopened.locator('#account-status').innerText(),'Connected · Pro');
    assert.match(await reopened.locator('#account-detail').innerText(),/sensor status/);
    reopened.on('dialog', d => d.accept());
    await reopened.locator('#account-disconnect').click();
    await reopened.locator('#account-signin').waitFor({state:'visible'});
    assert.equal(disconnects,1);
    console.log('PASS: actual extension popup sign-in, approval link, closed-popup completion, verified plan and disconnect');
  } finally { await context.close(); console.log('Disposable profile:',profile); }
})().catch(e => { console.error(e); process.exitCode=1; });
