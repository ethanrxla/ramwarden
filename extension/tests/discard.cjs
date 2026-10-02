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
  const context = vm.createContext({chrome,navigator:{userAgent:'Firefox'},console,Date,setInterval:noop,setTimeout:noop});
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
