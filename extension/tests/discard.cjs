const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');

function fixture(overrides = {}, fail = false, userAgent = "Firefox") {
  const noop = () => {};
  const event = {addListener: noop};
  let discards = [];
  let activated,removed;
  const now = Date.now();
  const tab = {id: 7, url:'https://example.org/article', active:false, pinned:false, audible:false,
    discarded:false, incognito:false, autoDiscardable:true, status:'complete', lastAccessed:now - 120*60000, ...overrides};
  const chrome = {
    runtime:{}, storage:{local:{get:(k,cb)=>cb({}),set:(v,cb)=>cb?.()},sync:{get:noop}},
    action:{setBadgeText:noop,setBadgeBackgroundColor:noop},
    tabs:{onActivated:{addListener:f=>activated=f},onUpdated:event,onRemoved:{addListener:f=>removed=f},query:noop,
      get:(id,cb)=>cb(tab), discard:(id,cb)=>{discards.push(id);if (!fail) tab.discarded=true;cb(fail ? undefined : tab);}},
  };
  const context = vm.createContext({chrome,navigator:{userAgent},console,Date,URL,Set,WebSocket:{OPEN:1},setInterval:noop,setTimeout:noop});
  vm.runInContext(fs.readFileSync('extension/background.js','utf8'),context);
  return {context,chrome,tab,discards,activate:x=>activated(x),remove:id=>removed(id)};
}
const command = {requestId:'r',minInactiveMinutes:45,tabs:[{id:7,url:'https://example.org/article'}]};
test('confirms successful discards only, keeps tabs open', async()=>{
  const f=fixture(); assert.deepEqual(Array.from(await f.context.discardTabs(command)),[7]);
});
test('fresh browser guards veto stale recommendation',async()=>{
  for (const override of [{active:true},{pinned:true},{audible:true},{discarded:true},{incognito:true},
    {autoDiscardable:false},{status:'loading'},{url:'https://example.org/new'},{lastAccessed:Date.now()}]) {
    const f=fixture(override);assert.equal((await f.context.discardTabs(command)).length,0);assert.equal(f.discards.length,0);
  }
});
test('failed or unavailable discard never claims success',async()=>{
  const f=fixture({},true);assert.equal((await f.context.discardTabs(command)).length,0);
  delete f.chrome.tabs.discard;assert.equal((await f.context.discardTabs(command)).length,0);
});
test('reports safety metadata and treats active tab as recently used',async()=>{
  const f=fixture({active:true});f.chrome.tabs.query=(_,cb)=>cb([f.tab]);
  const report=await f.context.buildTabReport({});assert.equal(report[0].active,true);
  assert.equal(report[0].inactiveMinutes,0);assert.equal(report[0].discardSupported,true);
});
test('batches are capped at five and partial results contain only confirmed IDs',async()=>{
  const f=fixture();const tabs=new Map(Array.from({length:6},(_,i)=>[i+1,{...f.tab,id:i+1}]));
  f.chrome.tabs.get=(id,cb)=>cb(tabs.get(id));
  f.chrome.tabs.discard=(id,cb)=>{f.discards.push(id);if(id%2)tabs.get(id).discarded=true;cb();};
  const result=await f.context.discardTabs({...command,tabs:Array.from(tabs.values(),t=>({id:t.id,url:t.url}))});
  assert.deepEqual(f.discards,[1,2,3,4,5]);assert.deepEqual(Array.from(result),[1,3,5]);
});

test('a sign-in word deep in the path no longer blocks an unload, but a sign-in path still does', async()=>{
  // These two used to be refused by the browser-side guard while the daemon listed
  // them as candidates, so nothing was ever unloaded.
  for (const url of ['https://claude.ai/design/v1/design/github/callback', 'https://sites.google.com/view/notebook-lm/login?authuser=0']) {
    const f=fixture({url});
    const cmd={requestId:'r',minInactiveMinutes:45,tabs:[{id:7,url}]};
    assert.deepEqual(Array.from(await f.context.discardTabs(cmd)),[7], url);
  }
  for (const url of ['https://example.org/login', 'https://example.org/oauth/authorize', 'https://example.org/session/new', 'chrome://extensions']) {
    const f=fixture({url});
    const cmd={requestId:'r',minInactiveMinutes:45,tabs:[{id:7,url}]};
    assert.equal((await f.context.discardTabs(cmd)).length,0, url);
    assert.equal(f.discards.length,0, url);
  }
});
test('Firefox tabs without autoDiscardable are still unloadable', async()=>{
  const f=fixture({autoDiscardable:undefined});
  assert.deepEqual(Array.from(await f.context.discardTabs(command)),[7]);
  f.chrome.tabs.query=(_,cb)=>cb([f.tab]);
  assert.equal((await f.context.buildTabReport({}))[0].autoDiscardable,true);
});
test('closing answers every command, skips guarded tabs and saves what closed', async()=>{
  for (const [override, expected] of [[{}, [7]], [{pinned:true}, []], [{url:'https://accounts.google.com/x'}, []]]) {
    const f=fixture(override); let saved=null; const removed=[];
    f.chrome.storage.local.get=(k,cb)=>cb({}); f.chrome.storage.local.set=(v,cb)=>{if (v.releasedTabs) saved=v.releasedTabs; cb?.();};
    f.chrome.tabs.remove=(id,cb)=>{removed.push(id);cb();};
    assert.deepEqual(Array.from(await f.context.closeTabs([7])), expected);
    assert.deepEqual(removed, expected);
    assert.equal(saved?.[0]?.url, expected.length ? f.tab.url : undefined);
  }
});
test('unloaded tabs are gathered into one collapsed Idle group, leaving user groups and pinned tabs', async()=>{
  const f=fixture(); const grouped=[]; const updates=[];
  const tabs=[
    {id:1,windowId:9,discarded:true,pinned:false,active:false,groupId:-1},
    {id:2,windowId:9,discarded:true,pinned:false,active:false,groupId:-1},
    {id:3,windowId:9,discarded:true,pinned:true,active:false,groupId:-1},
    {id:4,windowId:9,discarded:true,pinned:false,active:false,groupId:55},
    {id:5,windowId:9,discarded:false,pinned:false,active:true,groupId:-1},
  ];
  f.chrome.runtime.lastError=undefined;
  f.chrome.tabGroups={query:(_,cb)=>cb([{id:55,title:'DNS migration'}]), update:(id,p)=>updates.push([id,p])};
  f.chrome.tabs.group=(opts,cb)=>{grouped.push(opts);cb(77);};
  await f.context._tidyIdleGroups(tabs);
  assert.deepEqual(Array.from(grouped[0].tabIds),[1,2]);
  assert.equal(updates[0][0],77); assert.equal(updates[0][1].collapsed,true); assert.equal(updates[0][1].title,'Idle · 2');
});
test('a single unloaded tab does not get a group of its own', async()=>{
  const f=fixture(); let grouped=0;
  f.chrome.tabGroups={query:(_,cb)=>cb([]), update:()=>{}};
  f.chrome.tabs.group=()=>{grouped++;};
  await f.context._tidyIdleGroups([{id:1,windowId:9,discarded:true,pinned:false,active:false,groupId:-1}]);
  assert.equal(grouped,0);
});
test('a protected page left alone 4h can close; private hosts and in-tab editors never', async()=>{
  const old = Date.now() - 300*60000;
  for (const [url, lastAccessed, expected] of [
    ['https://console.tailscale.com/admin', old, [7]],
    ['https://login.microsoftonline.com/x', old, [7]],
    ['https://console.tailscale.com/admin', Date.now() - 30*60000, []],
    ['http://10.0.0.1/', old, []],
    ['https://vscode.dev/', old, []],
  ]) {
    const f=fixture({url, lastAccessed}); const removed=[];
    f.chrome.storage.local.get=(k,cb)=>cb({}); f.chrome.storage.local.set=(v,cb)=>cb?.();
    f.chrome.tabs.remove=(id,cb)=>{removed.push(id);cb();};
    assert.deepEqual(Array.from(await f.context.closeTabs([7])), expected, url);
  }
});

test('safe closing refuses navigation, recent activity, dirty work, protection and expired proposals',async()=>{
  for (const override of [{url:'https://example.org/new'},{lastAccessed:Date.now()},{active:true},{status:'loading'},{pinned:true}]) {
    const f=fixture(override);let removed=0;f.chrome.tabs.remove=(id,cb)=>{removed++;cb();};
    assert.equal((await f.context.closeSafe({...command,minInactiveMinutes:60,expiresAt:Date.now()+10000})).length,0);
    assert.equal(removed,0);
  }
  const f=fixture();f.chrome.tabs.remove=(id,cb)=>cb();
  vm.runInContext('safety.work[7]={url:"https://example.org/article",dirty:true}',f.context);
  assert.equal((await f.context.closeSafe({...command,minInactiveMinutes:60,expiresAt:Date.now()+10000})).length,0);
  vm.runInContext('safety.work={}',f.context);
  assert.equal((await f.context.closeSafe({...command,expiresAt:Date.now()-1})).length,0);
  await f.context.keepTabs(command.tabs);
  assert.equal((await f.context.closeSafe({...command,minInactiveMinutes:60,expiresAt:Date.now()+10000})).length,0);
});
test('safe closing confirms selected identity only and reopening protects a new tab ID',async()=>{
  const f=fixture();const removed=[];f.chrome.tabs.remove=(id,cb)=>{removed.push(id);cb();};
  assert.deepEqual(Array.from(await f.context.closeSafe({...command,minInactiveMinutes:60,expiresAt:Date.now()+10000})),[7]);
  assert.deepEqual(removed,[7]);
  f.context.observeReopen({...f.tab,id:9});
  assert.ok(vm.runInContext('safety.pages["https://example.org/article"].until>Date.now()',f.context));
});

test('safe closing never takes a work page but does take a browser settings page',async()=>{
  for (const url of ['https://cloud.oracle.com/db/adb','https://agensi.io/creators/submit-a-skill','https://ap.www.namecheap.com/domains/list/']) {
    const f=fixture({url});let removed=0;f.chrome.tabs.remove=(id,cb)=>{removed++;cb();};
    const cmd={requestId:'r',tabs:[{id:7,url}],minInactiveMinutes:60,expiresAt:Date.now()+10000};
    assert.equal((await f.context.closeSafe(cmd)).length,0,url);assert.equal(removed,0,url);
  }
  const url='chrome://extensions/';
  const f=fixture({url});f.chrome.tabs.remove=(id,cb)=>cb();
  assert.deepEqual(Array.from(await f.context.closeSafe({requestId:'r',tabs:[{id:7,url}],minInactiveMinutes:60,expiresAt:Date.now()+10000})),[7]);
  assert.equal(f.context.isDisposableBrowserPage('chrome-extension://abc/popup.html'),false);
});
test('reopening the same site at a different URL protects it, and every reopen counts',async()=>{
  const f=fixture();f.chrome.tabs.remove=(id,cb)=>cb();
  await f.context.closeSafe({...command,minInactiveMinutes:60,expiresAt:Date.now()+10000});
  f.context.observeReopen({...f.tab,id:9,url:'https://www.example.org/'});
  assert.ok(vm.runInContext('safety.sites["example.org"].until>Date.now()',f.context));
  assert.equal(vm.runInContext('safety.sites["example.org"].negative',f.context),1);
  f.chrome.tabs.query=(_,cb)=>cb([{...f.tab,id:9,url:'https://www.example.org/deals'}]);
  const report=await f.context.buildTabReport({});
  assert.ok(report[0].protectedUntilMs>Date.now(),'a different page on the reopened site is protected');
});

test('reports the running worker safe-close capability only for socket browsers', async()=>{
  for (const [agent,supported] of [['Chrome',true],['Firefox',false]]) {
    const f=fixture({},false,agent); f.chrome.tabs.query=(_,cb)=>cb([f.tab]);
    assert.equal((await f.context.buildTabReport({}))[0].safeCloseSupported,supported);
  }
});
test('safe close sends request-correlated confirmation including explicit refusal',async()=>{
  for (const active of [false,true]) {
    const f=fixture({active}); const sent=[];
    f.chrome.tabs.remove=(id,cb)=>cb(); f.context.sent=sent;
    vm.runInContext('ws = {readyState:1, send: data => sent.push(JSON.parse(data))}',f.context);
    await f.context.closeSafe({...command,minInactiveMinutes:60,expiresAt:Date.now()+10000});
    assert.deepEqual(JSON.parse(JSON.stringify(sent)),[{action:'tabs_closed_safe',requestId:'r',tabIds:active?[]:[7]}]);
  }
});

test('standalone Lite is opt-in, keeps the recent budget and only unloads eligible tabs',async()=>{
 const f=fixture();
 const active={...f.tab,id:1,active:true,lastAccessed:Date.now()};
 f.chrome.tabs.query=(_,cb)=>cb([active,f.tab]);
 f.chrome.tabs.get=(id,cb)=>cb(id===1?active:f.tab);
 f.chrome.storage.local.get=(keys,cb)=>cb({liteEnabled:false,liteBudget:1});
 assert.equal((await f.context.runStandaloneLite()).length,0);
 f.chrome.storage.local.get=(keys,cb)=>cb({liteEnabled:true,liteBudget:1});
 assert.deepEqual(Array.from(await f.context.runStandaloneLite()),[7]);
 assert.equal(active.discarded,false);
});
test('standalone Lite yields to a connected desktop daemon',async()=>{
 const f=fixture();f.context._badge(true);
 f.chrome.storage.local.get=(keys,cb)=>cb({liteEnabled:true,liteBudget:1});
 assert.equal((await f.context.runStandaloneLite()).length,0);assert.equal(f.discards.length,0);
});

test('standalone Lite rechecks opt-in immediately before unloading',async()=>{
 const f=fixture();const active={...f.tab,id:1,active:true,lastAccessed:Date.now()};f.chrome.tabs.query=(_,cb)=>cb([active,f.tab]);let reads=0;
 f.chrome.storage.local.get=(keys,cb)=>cb({liteEnabled:++reads===1,liteBudget:1});
 assert.equal((await f.context.runStandaloneLite()).length,0);assert.equal(f.discards.length,0);
});

test('explicit approval closes recent selected tabs but preserves live-work guards',async()=>{
  const url='chrome://extensions/';
  for (const extra of [{},{active:true},{pinned:true},{audible:true},{status:'loading'},{url:'https://example.org/navigated'}]) {
    const f=fixture({url,lastAccessed:Date.now(),...extra});const removed=[];
    f.chrome.tabs.remove=(id,cb)=>{removed.push(id);cb();};
    const result=await f.context.closeSafe({requestId:'approved',approved:true,tabs:[{id:7,url}],expiresAt:Date.now()+10000});
    assert.equal(result.length,Object.keys(extra).length===0?1:0);
  }
  const f=fixture({url});f.chrome.tabs.remove=(id,cb)=>cb();
  vm.runInContext('safety.work[7]={dirty:true}',f.context);
  assert.equal((await f.context.closeSafe({approved:true,tabs:[{id:7,url}],expiresAt:Date.now()+10000})).length,0);
});

test('reading an active tab persists recency after leaving and stale storage cannot rewind it',async()=>{
  const f=fixture({active:true,windowId:1,lastAccessed:Date.now()-7200000});
  f.chrome.tabs.query=(_,cb)=>cb([f.tab]);
  const old={7:{url:f.tab.url,lastActiveMs:Date.now()-7200000}};
  await f.context.buildTabReport(old);
  f.tab.active=false;
  const next=await f.context.buildTabReport(old);
  assert.equal(next[0].inactiveMinutes,0);
  assert.equal((await f.context.discardTabs(command)).length,0);
});

test('sleep gap is not counted as idle work and removal attribution is not user intent',async()=>{
 const f=fixture();
 vm.runInContext('tabActivity[7]={url:"https://example.org/article",lastActiveMs:Date.now()-7200000};lastActivitySample=Date.now()-7200000;accountForActivityGap(Date.now())',f.context);
 assert.ok(vm.runInContext('Date.now()-tabActivity[7].lastActiveMs<1000',f.context));
});

test('switching away stamps the end of a long reading session',async()=>{
 const f=fixture({active:true,windowId:3});f.chrome.tabs.query=(_,cb)=>cb([f.tab]);
 await f.context.buildTabReport({});
 vm.runInContext('tabActivity[7].lastActiveMs=Date.now()-7200000',f.context);
 f.activate({tabId:8,windowId:3});
 assert.ok(vm.runInContext('Date.now()-tabActivity[7].lastActiveMs<1000',f.context));
});
test('unattributed browser removal preserves keep feedback',()=>{
 const f=fixture();vm.runInContext('tabActivity[7]={url:"https://example.org/article"};safety.pages["https://example.org/article"]={until:Date.now()+600000}',f.context);
 f.remove(7);
 assert.ok(vm.runInContext('safety.pages["https://example.org/article"].until>Date.now()',f.context));
});
