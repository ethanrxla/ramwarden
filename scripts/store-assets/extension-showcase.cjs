// Real packaged extension UI + recorded, anonymized readings. Never opens a user profile.
const {chromium}=require('playwright');
const fs=require('node:fs'),path=require('node:path'),os=require('node:os'),assert=require('node:assert/strict');
(async()=>{
 const root=path.resolve(__dirname,'../..');process.chdir(root);
 const out=process.env.WARDEN_SHOWCASE_DIR||'/tmp/warden-store-review/showcase';fs.mkdirSync(out,{recursive:true});
 const data=process.env.WARDEN_RECORDED_DATA||'/tmp/warden-store-review/windows-recorded';
 const read=name=>JSON.parse(fs.readFileSync(path.join(data,name),'utf8').replace(/^\ufeff/,''));
 const state=read('state.json'),threat=read('threat.json'),ram=read('ram-report.json');
 const profile=fs.mkdtempSync(path.join(os.tmpdir(),'warden-showcase-'));
 const ext=path.join(profile,'extension');fs.cpSync('build/chrome',ext,{recursive:true});
 const background=path.join(ext,'background.js');fs.writeFileSync(background,fs.readFileSync(background,'utf8').replace('const DEFAULT_HOST = "localhost:7823";','const DEFAULT_HOST = "127.0.0.1:1";'));
 const context=await chromium.launchPersistentContext(profile,{headless:true,channel:'chromium',ignoreDefaultArgs:['--disable-extensions'],args:['--enable-unsafe-extension-debugging'],viewport:{width:640,height:800},recordVideo:{dir:path.join(out,'raw'),size:{width:640,height:800}}});
 let video;
 try{
  const cdp=await context.browser().newBrowserCDPSession();const loaded=await cdp.send('Extensions.loadUnpacked',{path:ext});
  const page=await context.newPage();video=page.video();const errors=[];page.on('pageerror',e=>errors.push(e.message));
  await page.route('http://localhost:7823/**',route=>{
   const endpoint=new URL(route.request().url()).pathname;
   const response=endpoint==='/stats'?{percent:state.percent,used_mb:state.used_mb,total_mb:state.total_mb,browsers_connected:state.browsers_connected}:endpoint==='/ram-report'?ram:endpoint==='/wardens/threat'?threat:endpoint==='/wardens'?{tier:threat.unlocked?'pro':'lite',paid:threat.unlocked?['ram','threat']:[]}:endpoint==='/history'?{closed_tabs:[]}:{};
   return route.fulfill({status:200,contentType:'application/json',body:JSON.stringify(response)});
  });
  await page.goto(`chrome-extension://${loaded.id}/popup.html`);
  await page.locator('#ram-pct').filter({hasText:Math.round(state.percent)+'%'}).waitFor();
  const shot=async name=>page.screenshot({path:path.join(out,name+'.png'),clip:{x:0,y:0,width:320,height:800}});
  await shot('extension-memory');await page.waitForTimeout(2500);
  await page.getByLabel('Browser Cleanup',{exact:true}).check();
  await page.getByLabel('Keep recent tabs').fill('8');await page.getByLabel('Keep recent tabs').press('Tab');
  await shot('extension-cleanup-settings');await page.waitForTimeout(3500);
  await page.getByRole('tab',{name:'ThreatWarden',exact:true}).click();await page.locator('#threat-engines').filter({hasText:'Sentinel'}).waitFor();
  await shot('extension-protection');await page.waitForTimeout(4500);
  if(await page.locator('.security-alert').count())await page.locator('#threat-alerts').evaluate(el=>el.scrollTop=el.scrollHeight);
  await page.waitForTimeout(2500);
  await page.getByRole('tab',{name:'RamWarden',exact:true}).click();await page.waitForTimeout(3000);
  assert.deepEqual(errors,[]);
  fs.writeFileSync(path.join(out,'provenance.json'),JSON.stringify({renderer:'real packaged Chrome extension UI',data:'recorded Windows device metrics; private evidence redacted',capturedAt:new Date().toISOString(),noUserProfile:true,noMemorySavingsInvented:true},null,2)+'\n');
 }finally{const movie=video?await video.path():null;await context.close();if(movie)fs.copyFileSync(movie,path.join(out,'extension-walkthrough.webm'));fs.rmSync(profile,{recursive:true,force:true});}
 const art=await chromium.launch({headless:true});
 try{
  const page=await art.newPage({viewport:{width:1280,height:800}});
  const ramSvg=fs.readFileSync('web/assets/pets/ram.svg').toString('base64');
  const threatSvg=fs.readFileSync('web/assets/pets/threat.svg').toString('base64');
  const shell=content=>`<html><body style="margin:0;width:1280px;height:800px;background:radial-gradient(ellipse at 10% 15%,#571b35,#130e1f 66%);color:#fff2f6;font-family:system-ui,sans-serif">${content}</body></html>`;
  const left=`<section style="position:absolute;left:65px;top:60px;width:740px"><div style="display:flex;align-items:center;gap:30px"><img src="data:image/svg+xml;base64,${ramSvg}" width="210" height="210" style="image-rendering:pixelated"><img src="data:image/svg+xml;base64,${threatSvg}" width="170" height="170" style="image-rendering:pixelated"></div><h1 style="font-size:55px;line-height:1.1;margin:15px 0 25px">For people who never<br>close their tabs.</h1><p style="font-size:25px;color:#dfc9e8;line-height:1.5">Choose your tab budget.<br>Keep active work protected.<br>Read your connected security report.</p><p style="font-size:15px;color:#bea7c8;margin-top:38px">Recorded device sample · private details redacted<br>Actual extension interface · no invented RAM savings</p></section>`;
  await page.setContent(shell(left));await page.screenshot({path:path.join(out,'extension-video-background.png')});
  for(const [raw,name] of [['extension-memory','extension-overview-1280x800'],['extension-cleanup-settings','extension-settings-1280x800'],['extension-protection','extension-threat-1280x800']]){
   const image=fs.readFileSync(path.join(out,raw+'.png')).toString('base64');
   await page.setContent(shell(left+`<img src="data:image/png;base64,${image}" style="position:absolute;left:900px;top:0;width:320px;height:800px;border-left:1px solid #ad688b">`));
   await page.screenshot({path:path.join(out,name+'.png')});
  }
 }finally{await art.close();}
 console.log('Exported real extension UI screenshots and walkthrough with recorded, redacted readings.');
})().catch(e=>{console.error(e);process.exitCode=1});
