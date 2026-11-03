// Session-backed header identity. No wallet detection or third-party avatars:
// having an extension installed does not mean the user signed into Warden.
import { makeApi, h, pageHref, shortAddress } from './common.js';

const paths = {
  email: 'M3 5h18v14H3z M3 6l9 7 9-7',
  wallet: 'M4 5h14v4H4z M3 9h18v11H3z M16 13h5v4h-5z',
  github: 'M9 20v-3c-4 1-4-2-6-2 M15 20v-4c0-1 1-2 1-2 4-1 5-6 2-8l-1-3-4 2h-2L7 3 6 6c-3 2-2 7 2 8 0 0 1 1 1 3',
};
function avatar(kind) {
  const span=h('span',{class:'account-avatar','aria-hidden':'true'});
  const svg=document.createElementNS('http://www.w3.org/2000/svg','svg');
  svg.setAttribute('viewBox','0 0 24 24');
  const path=document.createElementNS(svg.namespaceURI,'path');
  path.setAttribute('d',paths[kind] || paths.github);
  svg.append(path);span.append(svg);
  return span;
}
export function accountLabel(account) {
  const identity=account?.identity;
  if(identity?.kind==='email') return {kind:'email',label:identity.email};
  if(identity?.kind==='wallet') return {kind:'wallet',label:shortAddress(identity.address),full:identity.address};
  if(identity?.kind==='github') return {kind:'github',label:'GitHub account'};
  return null;
}
export class HeaderAccount {
  constructor(link,api) {this.link=link;this.api=api;this.revision=0;this.pending=false;}
  render(account,unknown=false) {
    const identity=accountLabel(account);
    this.link.replaceChildren();
    this.link.href=pageHref('account.html#sign-in-methods');
    this.account=account;
    this.closeDetails();
    this.link.classList.toggle('account-connected',!!identity);
    this.link.dataset.accountState=identity?'signed-in':unknown?'unknown':'signed-out';
    if(!identity) {
      this.link.textContent=unknown?'Account':'Sign in · Wallet or email';
      this.link.removeAttribute('aria-label');this.link.removeAttribute('title');return;
    }
    this.link.setAttribute('aria-label',`Account: ${identity.label} · signed in`);
    this.link.title=`Signed in as ${identity.full || identity.label}. Open account and devices.`;
    this.link.append(avatar(identity.kind),h('span',{class:'account-identity'},identity.label));
    if(account.plan==='plus' || account.tier==='ram' && ['trial','active','grace'].includes(account.status)) this.link.append(h('span',{class:'account-plan-badge',title:'RamWarden Plus','aria-label':'Plus'},'⚡'));
    if(account.plan==='pro' || account.tier==='pro' && ['trial','active','grace'].includes(account.status)) this.link.append(h('span',{class:'account-plan-badge',title:'RamWarden Pro','aria-label':'Pro'},'🐐'));
    this.link.append(h('span',{class:'account-online','aria-hidden':'true'}));
  }
  closeDetails() { this.details?.remove();this.details=null;this.link.setAttribute('aria-expanded','false'); }
  toggleDetails(event) {
    if(!this.account) return;
    event.preventDefault();
    if(this.details) {this.closeDetails();return;}
    const identities=this.account.identities || [this.account.identity];
    const panel=h('section',{class:'account-details','aria-label':'Connected sign-in methods'},h('strong',{},'Your connected account'));
    for(const identity of identities) {
      const item=accountLabel({identity});
      if(item) panel.append(h('div',{class:'account-method'},avatar(item.kind),h('span',{},item.full || item.label)));
    }
    panel.append(h('a',{href:pageHref('account.html#sign-in-methods')},'Manage account & devices →'));
    this.link.parentElement.append(panel);this.details=panel;this.link.setAttribute('aria-expanded','true');
  }
  async refresh() {
    if(this.pending) return;
    this.pending=true;const revision=this.revision;
    try {const account=await this.api.account();if(revision===this.revision)this.render(account);}
    catch(error) {if(revision===this.revision)this.render(null,error.status!==401);}
    finally {this.pending=false;}
  }
  start() {
    this.link.classList.add('account-header');this.link.dataset.accountLink='';
    this.link.parentElement.append(this.link);
    this.link.addEventListener('click',event=>this.toggleDetails(event));
    document.addEventListener('click',event=>{if(!this.link.contains(event.target)&&!this.details?.contains(event.target))this.closeDetails();});
    document.addEventListener('keydown',event=>{if(event.key==='Escape')this.closeDetails();});
    this.render(null,true);
    document.addEventListener('warden:account',event=>{this.revision++;this.render(event.detail);});
    addEventListener('pageshow',()=>this.refresh());
    document.addEventListener('visibilitychange',()=>{if(!document.hidden)this.refresh();});
    this.refresh();
  }
}
if(typeof document!=='undefined') {
 const link=document.querySelector('.site-header nav:last-child a[href^="account.html"]');
 if(link) makeApi().then(api=>new HeaderAccount(link,api).start());
}
