// This module never receives a device token or stores account credentials.
function localAccountHost(base) {
  try {
    const u = new URL(base);
    return u.protocol === 'http:' && ['localhost', '127.0.0.1', '[::1]'].includes(u.hostname)
      && !u.username && !u.password && u.pathname === '/' && !u.search && !u.hash;
  } catch { return false; }
}

function accountApprovalURL(raw) {
  try {
    const u = new URL(raw);
    const local = ['localhost', '127.0.0.1', '[::1]'].includes(u.hostname);
    return (u.protocol === 'https:' || (u.protocol === 'http:' && local))
      && !u.username && !u.password && u.pathname === '/link.html' && !u.hash
      && !!u.searchParams.get('code') ? u.href : null;
  } catch { return null; }
}

function accountView(s) {
  if (s?.status === 'unverified_device') return {
    text: 'Existing device sign-in needs attention', detail: s.message, linked: true,
  };
  if (s?.status === 'pending') return {
    text: `Approve this computer on the sign-in page. Code: ${s.user_code || ''}`,
    detail: 'Email and wallet sign-in are available on that page. You can close this popup while signing in.',
    pending: true, url: accountApprovalURL(s.verification_uri_complete),
  };
  if (s?.status === 'linked') return {
    text: s.verified ? `Connected · ${s.tier === 'pro' ? 'Pro' : s.tier === 'ram' ? 'Plus' : 'Lite'}` : 'Connected · plan not verified',
    detail: s.message || (s.threat_unlocked ? 'ThreatWarden access verified. Check the app for sensor status.' : 'ThreatWarden is locked on this computer.'),
    linked: true,
  };
  return { text: 'This app is signed out.', detail: s?.message || 'Link this browser and the local app to the same account.', signedOut: true };
}

function mountAccount({request, base, open}) {
  const el = id => document.getElementById(id);
  let approval, timer;
  const render = state => {
    const v = accountView(state); approval = v.url;
    el('account-status').textContent = v.text;
    el('account-detail').textContent = v.detail;
    el('account-signin').hidden = !v.signedOut;
    el('account-continue').hidden = !approval;
    el('account-refresh').hidden = !v.linked;
    el('account-disconnect').hidden = !(v.linked || v.pending);
    clearTimeout(timer);
    if (v.pending) timer = setTimeout(() => run('/account'), 5000);
  };
  const run = async (path, method = 'GET', launch = false) => {
    try {
      if (!localAccountHost(await base())) throw new Error('Sign in through the app on this computer. Remote daemon sign-in is unavailable.');
      const state = await request(path, {method, signal: AbortSignal.timeout(15000)});
      render(state);
      if (launch && approval) await open(approval);
    } catch (e) {
      clearTimeout(timer);
      el('account-status').textContent = e.message === '404' ? 'Update the local app to enable account sign-in.' : 'Account connection unavailable.';
      el('account-detail').textContent = e.message;
      for (const id of ['signin', 'continue', 'refresh', 'disconnect']) el('account-' + id).hidden = true;
    }
  };
  el('account-signin').addEventListener('click', () => run('/account/start', 'POST', true));
  el('account-continue').addEventListener('click', () => { if (approval) open(approval); });
  el('account-refresh').addEventListener('click', () => run('/account/refresh', 'POST'));
  el('account-disconnect').addEventListener('click', () => {
    if (confirm('Disconnect this app? Your device slot remains registered. Remove it on your account page to free the slot.')) run('/account/disconnect', 'POST');
  });
  run('/account');
}

if (typeof module !== 'undefined') module.exports = { localAccountHost, accountApprovalURL, accountView };
