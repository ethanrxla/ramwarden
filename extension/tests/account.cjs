const {test} = require('node:test');
const assert = require('node:assert/strict');
const {localAccountHost, accountApprovalURL, accountView} = require('../account.js');

test('account operations are limited to this computer', () => {
  for (const host of ['localhost', '127.0.0.1', '[::1]']) assert.ok(localAccountHost(`http://${host}:7823`));
  for (const host of ['http://100.77.123.96:7823', 'http://localhost.evil.test', 'http://user@localhost', 'http://localhost/path']) assert.equal(localAccountHost(host), false);
});
test('approval links refuse script URLs, credentials and insecure remote sites', () => {
  assert.equal(accountApprovalURL('https://ramwarden.com/link.html?code=AAAA-BBBB'), 'https://ramwarden.com/link.html?code=AAAA-BBBB');
  assert.ok(accountApprovalURL('http://localhost:8000/link.html?code=AAAA-BBBB'));
  for (const url of ['javascript:alert(1)', 'http://example.org/link.html?code=X', 'https://user:pass@example.org/link.html?code=X', 'https://example.org/link.html', 'https://example.org/other?code=X']) assert.equal(accountApprovalURL(url), null);
});
test('linked without a verified plan never promises threat coverage', () => {
  assert.equal(accountView({status:'unverified_device'}).linked, true, 'legacy credential can be disconnected');
  const v = accountView({status:'linked', verified:false, tier:'pro', device_token:'should-never-be-displayed'});
  assert.match(v.text, /not verified/);
  assert.doesNotMatch(JSON.stringify(v), /should-never-be-displayed/);
  assert.match(accountView({status:'linked', verified:true, tier:'pro', threat_unlocked:true}).detail, /Check the app for sensor status/);
});
