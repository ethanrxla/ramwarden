// Reports that the page is in use, and (v2) whether text the user typed is
// still sitting unsent in a field. Only true or false leaves the page: no page
// text, no field names. The background holds a tab with unsent text; a search
// that was run or a chat message that was sent no longer counts as work.
//
// Text counts as sent only when it is gone: the field was emptied or removed,
// or the page moved on. Pressing Enter or submitting proves nothing: required
// fields can block the submit and a save can fail, with the text still there.
(() => {
  let last=0, sent=null, timer=0;
  const edited=new Map(); // field -> location.href when it was typed in
  const TEXT_TYPES=new Set(["text","email","url","tel","number",""]);
  const LOOKUP=/search|filter|find|query/i;
  // A field whose text is work: not a password, not a search or filter box.
  function holdsWork(el) {
    if (!el || el.nodeType!==1) return false;
    if (el.tagName==="TEXTAREA" || el.isContentEditable) return true;
    if (el.tagName!=="INPUT" || !TEXT_TYPES.has((el.getAttribute("type")||"").toLowerCase())) return false;
    const role=el.getAttribute("role")||"";
    return role!=="searchbox" && role!=="combobox"
      && !LOOKUP.test(`${el.name||""} ${el.id||""} ${el.getAttribute("placeholder")||""} ${el.getAttribute("aria-label")||""}`);
  }
  const text=el=>(el.isContentEditable ? el.textContent : el.value) || "";
  function unsent() {
    for (const [el,href] of edited) {
      if (!el.isConnected || href!==location.href || !text(el).trim()) edited.delete(el);
    }
    return edited.size>0;
  }
  function send(dirty) {
    last=Date.now(); sent=dirty;
    try { chrome.runtime.sendMessage({type:"rw_activity",v:2,dirty})?.catch?.(()=>{}); } catch { /* extension reloaded */ }
  }
  function report(event) {
    if (!event.isTrusted || document.visibilityState!=="visible") return;
    const el=event.target;
    if (event.type==="input" && holdsWork(el)) edited.set(el,location.href);
    // Apps empty or remove the field a moment after the event: look again then.
    if (!timer) timer=setTimeout(()=>{ timer=0; const dirty=unsent(); if (dirty!==sent) send(dirty); },1500);
    const dirty=unsent();
    if (dirty===sent && Date.now()-last<5000) return;
    send(dirty);
  }
  for (const type of ["pointerdown","keydown","scroll","input"]) document.addEventListener(type,report,{capture:true,passive:true});
})();
