(() => {
  let last=0;
  function report(event) {
    if (!event.isTrusted || document.visibilityState!=="visible") return;
    const dirty=event.type==="input";
    if (!dirty && Date.now()-last<5000) return;
    last=Date.now();
    chrome.runtime.sendMessage({type:"rw_activity",dirty})?.catch?.(()=>{});
  }
  for (const type of ["pointerdown","keydown","scroll","input"]) document.addEventListener(type,report,{capture:true,passive:true});
})();
