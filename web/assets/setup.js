import { h, makeApi, show, notice } from './common.js';
import { InstallBox } from './install.js';
export function setupPrompt(limit) {
  const count = [1,3,5].includes(limit) ? limit : 1;
  return `Help me set up RamWarden on up to ${count} of my computers, within my account's remaining allowance. Use the official RamWarden agent skill at https://github.com/ethanrxla/ramwarden/blob/main/agent/skills/ramwarden/SKILL.md. First list the computers I choose, their operating systems, and what is already installed. Use only remote access I have authorized. Update existing installations in place and preserve settings, credentials, and history. Sign each computer into my account separately through its own device approval; never reuse an install link or copy device tokens, fingerprints, or passes between computers. Ask me to approve any new remote access. Verify the daemon, companion, and browser extension on each computer and report what works. Do not buy a plan or change network/security settings without my approval.`;
}
async function start() {
 const root=document.querySelector('#flow');
 const api=await makeApi();
 let account=null;
 try { account=await api.account(); } catch { /* Sign-in is the first step. */ }
 const limit=Number(account?.device_limit) || (account?.tier === 'pro' ? 5 : account?.tier === 'ram' ? 3 : 1);
 const install=h('div');
 const steps=[
  h('section',{},h('h2',{},'1. Connect your account'),h('p',{},'Use the same account on every computer. Plus covers up to 3 computers; Pro covers up to 5. Each computer gets its own approval.'),h('a',{class:'button',href:'account.html'},account?'Manage account and devices':'Sign in with wallet or email')),
  h('section',{},h('h2',{},'2. Install or update'),h('p',{},'Choose this computer’s system below. Keep your existing settings when updating.'),install),
  h('section',{},h('h2',{},'3. Connect your browser'),h('p',{},'Enable the RamWarden browser extension in each profile you use. The companion can only manage tabs in a connected profile.'),h('a',{href:'https://github.com/ethanrxla/ramwarden/tree/main/extension'},'Browser extension setup')),
  h('section',{},h('h2',{},'4. Make it yours'),h('p',{},'Open Customize your ram from the menu bar or tray. Choose its name, colors, eyes, and size. Check the companion’s connection before asking it to manage tabs.')),
  h('section',{},h('h2',{},'5. Set up your other computers with your agent'),h('p',{},'Copy this request into your own coding assistant. It contains no account keys or personal install links.'),h('textarea',{readonly:true,rows:'9','aria-label':'Agent setup request'},setupPrompt(limit)))
 ];
 const copy=h('button',{class:'button',type:'button',onclick:async()=>{try {await navigator.clipboard.writeText(setupPrompt(limit));copy.textContent='Copied';}catch{copy.textContent='Select the text above to copy';}}},'Copy agent setup request');
 steps[4].append(copy);
 show(root,h('h1',{},'A little setup. A lot more breathing room.'),h('p',{},'Follow these steps on each computer. Tick a step when you have completed it; these checks are your notes, not an automatic system test.'),...steps);
 steps.forEach((step,index)=>step.append(h('label',{class:'setup-check'},h('input',{type:'checkbox','aria-label':`I completed step ${index+1}`}), ' Done')));
 await new InstallBox(install,{api,account}).render();
}
if(typeof document!=='undefined') start().catch(()=>show(document.querySelector('#flow'),notice('error','Setup could not load. Refresh or open your account page.')));
