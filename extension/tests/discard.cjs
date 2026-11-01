const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');

function fixture(overrides = {}, fail = false) {
  const noop = () => {};
  const event = {addListener: noop};
  let discards = [];
  const now = Date.now();
  const tab = {id: 7, url:'https://example.org/article', active:false, pinned:false, audible:false,
    discarded:false, incognito:false, autoDiscardable:true, status:'complete', lastAccessed:now - 120*60000, ...overrides};
  const chrome = {
    runtime:{}, storage:{local:{get:noop,set:noop},sync:{get:noop}},
    action:{setBadgeText:noop,setBadgeBackgroundColor:noop},
    tabs:{onActivated:event,onUpdated:event,onRemoved:event,query:noop,
      get:(id,cb)=>cb(tab), discard:(id,cb)=>{discards.push(id);if (!fail) tab.discarded=true;cb(fail ? undefined : tab);}},
  };
  const context = vm.createContext({chrome,navigator:{userAgent:'Firefox'},console,Date,URL,setInterval:noop,setTimeout:noop});
  vm.runInContext(fs.readFileSync('extension/background.js','utf8'),context);
  return {context,chrome,tab,discards};
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
