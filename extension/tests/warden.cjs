const { test } = require('node:test');
const assert = require('node:assert/strict');
const { wardenView, wardenHTML, analysisHTML } = require('../warden.js');

const checkIn = { pet: true, muted: false, bubble: { id: 'b1', kind: 'check_in', text: 'Coming back to "<Docs>"?',
  choices: [{ id: 'keep', label: 'Yes, keep it' }, { id: 'close', label: 'No, close it' }, { id: 'later', label: 'Ask me later' }],
  tab: { browser: 'x', id: 3, url: 'https://example.org/' } } };

test('a check-in shows its three choices and escapes the tab title', () => {
  const v = wardenView(checkIn);
  assert.equal(v.question, true);
  const html = wardenHTML(v);
  assert.match(html, /data-choice="keep"/);
  assert.match(html, /class="warden-choice danger" data-choice="close"/);
  assert.match(html, /data-choice="dismiss"/);
  assert.ok(html.includes('&lt;Docs&gt;') && !html.includes('<Docs>'));
});

test('no pet on Lite, nothing to say, or an old daemon: the warden is hidden', () => {
  assert.equal(wardenView({ ...checkIn, pet: false }), null);
  assert.equal(wardenView({ pet: true, bubble: null, muted: false }), null);
  assert.equal(wardenView(null), null);
});

test('muted shows a quiet line with an unmute button and no mute menu', () => {
  const v = wardenView({ pet: true, bubble: null, muted: true });
  const html = wardenHTML(v);
  assert.match(html, /keeping quiet/);
  assert.match(html, /data-choice="unmute"/);
  assert.doesNotMatch(html, /warden-mute/);
});

test('analysis offers only the suggested actions and marks a rules-only answer', () => {
  const html = analysisHTML({ reply: 'Two feeds can go.', tier: 'facts', unload: [{}, {}], close: [] });
  assert.match(html, /Unload 2 tabs/);
  assert.doesNotMatch(html, /data-act="close"/);
  assert.match(html, /Rules only/);
  assert.doesNotMatch(analysisHTML({ reply: 'x', tier: 'cloud' }), /Rules only|data-act/);
});
