const { test } = require('node:test');
const assert = require('node:assert/strict');
const { wardenView, wardenHTML, analysisHTML, focusHTML, storageHTML, answerText } = require('../warden.js');

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

test('the focus question is a text box, not a "Tell Warden" button', () => {
  const now = wardenView({ pet: true, muted: false, bubble: { id: 'f', kind: 'focus', text: 'What are you working on?', choices: [], input: 'What are you working on?' } });
  const html = wardenHTML(now);
  assert.match(html, /<form class="warden-say"><input class="warden-input"/);
  assert.match(html, /placeholder="What are you working on\?"/);
  assert.doesNotMatch(html, /Tell Warden/);
  // An older daemon's button is dropped for the same box.
  const old = wardenView({ pet: true, muted: false, bubble: { id: 'f', kind: 'focus', text: '?', choices: [{ id: 'focus', label: 'Tell Warden' }] } });
  assert.deepEqual(old.buttons, []);
  assert.equal(old.input, 'What are you working on?');
  assert.ok(old.question);
});

test('a focus reply lists its tabs as checkboxes and escapes them', () => {
  const html = focusHTML('<b>thesis</b>', { reply: 'On it.', proposal: { token: 't', tabs: [
    { browser: 'chrome', id: 1, url: 'https://a/', title: '<i>Docs</i>', selected: true },
    { browser: 'chrome', id: 2, url: 'https://b/', title: '', selected: false }] } });
  assert.match(html, /You: &lt;b&gt;thesis&lt;\/b&gt;/);
  assert.match(html, /data-tab="0" checked> &lt;i&gt;Docs/);
  assert.match(html, /data-tab="1"> Web page/);
  assert.match(html, /data-act="focus-keep"/);
  assert.doesNotMatch(focusHTML('x', { reply: 'No tabs.', proposal: null }), /focus-keep/);
});

test('a storage answer is never silent', () => {
  assert.equal(answerText('free', 200, { reply: 'Done. I cleared 0.1 GB.' }), 'Done. I cleared 0.1 GB.');
  assert.equal(answerText('free', 200, {}), 'Done.');
  assert.match(answerText('show', 500, {}), /answered 500/);
  assert.equal(answerText('dismiss', 200, { reply: '' }), '');
});

test('"What\'s using it?" draws the biggest folders and keeps the offer', () => {
  const usage = { home_folders: [{ name: 'Library', bytes: 4e10, size: '40 GB', complete: false }, { name: 'tiny', bytes: 10, size: '0 GB', complete: true }],
    places: [{ label: 'Browser caches', kind: 'clear', bytes: 1e8, size: '0.1 GB', complete: true }], not_measured: ['Desktop', 'Documents'] };
  const html = storageHTML('Macintosh HD has 20 GB free.', usage, { id: 'b2', buttons: [{ id: 'free', label: 'Free 0.1 GB' }] });
  assert.match(html, /Library<\/span><b>≥ 40 GB/);
  assert.doesNotMatch(html, /tiny/);
  assert.match(html, /Browser caches \(I can clear\)/);
  assert.match(html, /Not measured: Desktop, Documents/);
  assert.match(html, /data-choice="free"/);
  assert.match(html, /data-act="storage-done"/);
});
