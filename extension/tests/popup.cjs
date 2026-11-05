// Disposable browser and synthetic reports only; never opens a user's profile.
const {chromium}=require('playwright');const fs=require('node:fs');const os=require('node:os');const path=require('node:path');const assert=require('node:assert/strict');
(async()=>{
 const profile=fs.mkdtempSync(path.join(os.tmpdir(),'warden-popup-'));const ext=path.join(profile,'extension');fs.cpSync(path.resolve('build/chrome'),ext,{recursive:true});const bg=path.join(ext,'background.js');fs.writeFileSync(bg,fs.readFileSync(bg,'utf8').replace('const DEFAULT_HOST = "localhost:7823";','const DEFAULT_HOST = "127.0.0.1:1";'));
 const context=await chromium.launchPersistentContext(profile,{headless:true,channel:'chromium',ignoreDefaultArgs:['--disable-extensions'],args:['--enable-unsafe-extension-debugging'],viewport:{width:640,height:800}});
 try{
  const cdp=await context.browser().newBrowserCDPSession();const loaded=await cdp.send('Extensions.loadUnpacked',{path:ext});const page=await context.newPage();
  let offline=false;let paid=true;const errors=[];page.on('pageerror',e=>errors.push(e.message));
  await page.route('http://localhost:7823/**',route=>{
   if(offline)return route.abort();const pathname=new URL(route.request().url()).pathname;
   const alert={timestamp:'2026-11-03T13:22:00Z',rule_id:'550',level:7,description:'Demo: a monitored configuration file changed.'};
   const body=pathname==='/stats'?{percent:67,used_mb:10977,total_mb:16384,browsers_connected:1}:pathname==='/wardens/threat'?{unlocked:true,status:{headline:'Security events to review',detail:'Demonstration report. No real device data.'},report:{engines:[{name:'Sentinel',state:'on'},{name:'Antivirus',state:'on'}],alerts:Array.from({length:60},(_,i)=>({...alert,rule_id:String(550+i)}))}}:pathname==='/history'?{closed_tabs:[]}:pathname==='/wardens'?{paid:paid?['ram','threat']:[],tier:'pro'}:{};
   return route.fulfill({status:200,contentType:'application/json',body:JSON.stringify(body),headers:{'Access-Control-Allow-Origin':'*'}});
  });
  await page.goto(`chrome-extension://${loaded.id}/popup.html`);
  await page.getByText('Pro 🐐 · Active on this computer',{exact:true}).waitFor();
  assert.equal(await page.locator('header img').getAttribute('src'),'icons/icon48.png');
  paid=false;await page.getByRole('button',{name:'Check connection',exact:true}).click();
  await page.getByText('Lite · No active paid pass on this computer',{exact:true}).waitFor();
  assert.match(await page.locator('#account-help').innerText(),/ramwarden login/);
  paid=true;await page.getByRole('button',{name:'Check connection',exact:true}).click();await page.getByText('Pro 🐐 · Active on this computer',{exact:true}).waitFor();
  await page.getByRole('tab',{name:'ThreatWarden',exact:true}).click();await page.waitForSelector('.security-alert');assert.equal(await page.locator('.security-alert').count(),50);assert.equal(await page.locator('#ram-pane').isHidden(),true);
  fs.mkdirSync('/tmp/warden-store-review',{recursive:true});await page.screenshot({path:'/tmp/warden-store-review/threat-pane.png'});
  const ui=(await page.screenshot({clip:{x:0,y:0,width:320,height:680}})).toString('base64');
  const icon=fs.readFileSync('assets/brand/icon128.png').toString('base64');
  const promo=await context.newPage();await promo.setViewportSize({width:1280,height:800});
  await promo.setContent(`<html><body style="margin:0;background:radial-gradient(ellipse at 20% 10%,#4b1734,#100d19 65%);color:#f9eff4;font-family:system-ui,sans-serif"><main style="display:flex;align-items:center;justify-content:space-between;padding:50px 80px;gap:80px"><section style="max-width:590px"><img src="data:image/png;base64,${icon}" width="96" height="96" style="image-rendering:pixelated"><p style="letter-spacing:4px;color:#ff8798;font-size:17px">RAMWARDEN</p><h1 style="font-size:54px;line-height:1.1;margin:25px 0">Keep the tabs.<br>Lose the weight.</h1><p style="font-size:23px;line-height:1.5;color:#d0c1db">Free, reversible idle-tab unloading.<br>Security reports from your connected desktop app.</p><p style="font-size:15px;color:#ad9eba;margin-top:38px">Optional cleanup. No automatic payments.<br>Product preview with synthetic demonstration data.</p></section><img src="data:image/png;base64,${ui}" width="320" height="680" style="border:1px solid #715070;border-radius:16px;box-shadow:0 25px 80px #0008"></main></body></html>`);
  await promo.screenshot({path:'/tmp/warden-store-review/1-threat-pane-1280x800.png'});await promo.close();
  await page.getByRole('tab',{name:'RamWarden',exact:true}).click();
  const ram=(await page.screenshot({clip:{x:0,y:0,width:320,height:680}})).toString('base64');
  const ramPromo=await context.newPage();await ramPromo.setViewportSize({width:1280,height:800});
  await ramPromo.setContent(`<html><body style="margin:0;background:radial-gradient(ellipse at 15% 10%,#581a32,#100d19 70%);color:#f9eff4;font-family:system-ui,sans-serif"><main style="display:flex;align-items:center;justify-content:space-between;padding:50px 80px;gap:70px"><section style="max-width:600px"><img src="data:image/png;base64,${icon}" width="128" height="128" style="image-rendering:pixelated"><h1 style="font-size:51px;line-height:1.12">For people who never close their tabs.</h1><p style="font-size:23px;line-height:1.5;color:#d0c1db">Reversible browser cleanup.<br>Your recent work stays protected.</p><p style="font-size:15px;color:#ad9eba;margin-top:35px">Browser Cleanup works without an account.<br>RAM reports require the desktop app.<br>Synthetic demonstration data.</p></section><img src="data:image/png;base64,${ram}" width="320" height="680" style="border:1px solid #715070;border-radius:16px;box-shadow:0 25px 80px #0008"></main></body></html>`);
  await ramPromo.screenshot({path:'/tmp/warden-store-review/0-browser-cleanup-1280x800.png'});await ramPromo.close();
  const tile=await context.newPage();
  for(const [width,height,name] of [[440,280,'small-promo-440x280'],[1400,560,'marquee-1400x560']]) {
   await tile.setViewportSize({width,height});
   await tile.setContent(`<html><body style="margin:0;width:100vw;height:100vh;display:grid;place-items:center;background:radial-gradient(circle,#542149,#171021 62%);overflow:hidden"><div style="position:absolute;width:${height*.9}px;height:${height*.9}px;border:1px solid #dc425f77;border-radius:50%;box-shadow:0 0 0 ${height*.12}px #9d39591c,0 0 0 ${height*.24}px #75519612"></div><img src="data:image/png;base64,${icon}" style="width:${height*.85}px;height:${height*.85}px;image-rendering:pixelated;position:relative"></body></html>`);
   await tile.screenshot({path:'/tmp/warden-store-review/'+name+'.png'});
  }
  await tile.close();

  offline=true;await page.getByRole('button',{name:'Check connection',exact:true}).click();await page.getByText('Lite · Desktop connection unavailable',{exact:true}).waitFor();await page.getByRole('button',{name:'Refresh security report'}).click();await page.getByText('Protection unverified',{exact:true}).waitFor();assert.equal(await page.locator('.security-alert').count(),0,'offline refresh must remove stale alerts');
  await page.getByRole('tab',{name:'RamWarden',exact:true}).click();await page.getByLabel('Browser Cleanup').check();await page.getByLabel('Keep recent tabs').fill('8');await page.getByLabel('Keep recent tabs').press('Tab');
  assert.deepEqual(errors,[]);console.log('PASS: Threat pane, 50-alert cap, stale-report clearing, Lite settings and popup CSP.');
 }finally{await context.close();fs.rmSync(profile,{recursive:true,force:true});}
})().catch(e=>{console.error(e);process.exitCode=1});
