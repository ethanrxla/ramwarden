const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');

// A page as activity.js sees it: fields, events and what it tells the background.
function page() {
  const listeners = {}, messages = [], timers = [];
  let now = 1_000_000;
  const document = {visibilityState:'visible', addEventListener:(type,f)=>{listeners[type]=f;}};
  const location = {href:'https://example.org/form'};
  const chrome = {runtime:{sendMessage:m=>{messages.push(m);return Promise.resolve();}}};
  const context = vm.createContext({document, location, chrome, Map, Set, Date:{now:()=>now},
    setTimeout:f=>{timers.push(f);return timers.length;}});
  vm.runInContext(fs.readFileSync('extension/activity.js','utf8'), context);
  const field = (tagName, attrs={}) => ({nodeType:1, tagName, isConnected:true, isContentEditable:false, value:'', textContent:'',
    getAttribute:k=>attrs[k] ?? null, name:attrs.name||'', id:attrs.id||'', contains:()=>false, ...attrs.props});
  return {
    field, location, document, messages,
    fire:(type,target,extra={})=>listeners[type]({type,target,isTrusted:true,...extra}),
    later:(ms=6000)=>{ now+=ms; for (const f of timers.splice(0)) f(); },
    last:()=>messages.at(-1)?.dirty,
  };
}

test('typed text is unsent work until the field is emptied, removed or the page moves on', () => {
  const p = page(), box = p.field('TEXTAREA');
  box.value = 'half a message'; p.fire('input', box);
  assert.deepEqual({...p.messages.at(-1)}, {type:'rw_activity', v:2, dirty:true});
  box.value = ''; p.fire('keydown', box, {key:'Enter'}); p.later();
  assert.equal(p.last(), false, 'the app sent it and emptied the box');
  box.value = 'another'; p.fire('input', box); assert.equal(p.last(), true);
  box.isConnected = false; p.fire('pointerdown', {nodeType:1}); p.later();
  assert.equal(p.last(), false, 'the field is gone');
  const next = p.field('TEXTAREA'); next.value = 'draft'; p.fire('input', next); assert.equal(p.last(), true);
  p.location.href = 'https://example.org/done'; p.later(); p.fire('scroll', {nodeType:1});
  assert.equal(p.last(), false, 'an in-page navigation carried the work away');
});

// Enter or a submit attempt proves nothing: a required field can block the
// submit and a save can fail, with the text still in the form.
test('text stays unsent work through Enter and a blocked submit, until it is actually gone', () => {
  const p = page();
  const name = p.field('INPUT', {type:'text', name:'full-name'}); name.value = 'Ada';
  p.fire('input', name); assert.equal(p.last(), true);
  p.fire('keydown', name, {key:'Enter'}); p.later();
  assert.equal(p.last(), true, 'validation blocked the submit; the name is still there');
  p.fire('pointerdown', {nodeType:1}); p.later();
  assert.equal(p.last(), true, 'a failed save leaves it unsent too');
  name.value = ''; p.fire('pointerdown', {nodeType:1}); p.later();
  assert.equal(p.last(), false, 'the app cleared the form after saving');
});

test('a search, a filter, a password and a checkbox are not unsent work', () => {
  const p = page();
  for (const attrs of [{type:'search'}, {type:'text', placeholder:'Search issues'}, {type:'text', role:'combobox'}, {type:'password'}, {type:'checkbox'}]) {
    const f = p.field('INPUT', attrs); f.value = 'x'; p.later(); p.fire('input', f);
    assert.equal(p.last(), false, JSON.stringify(attrs));
  }
});

test('hidden pages and synthetic events report nothing, and idle reports are throttled', () => {
  const p = page();
  p.fire('scroll', {nodeType:1}, {isTrusted:false});
  p.document.visibilityState = 'hidden'; p.fire('scroll', {nodeType:1});
  assert.equal(p.messages.length, 0);
  p.document.visibilityState = 'visible';
  p.fire('scroll', {nodeType:1}); p.fire('scroll', {nodeType:1});
  assert.equal(p.messages.length, 1);
  p.later(); p.fire('scroll', {nodeType:1}); assert.equal(p.messages.length, 2);
});
