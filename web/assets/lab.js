// Illustrative simulator, not telemetry or benchmark data. No network requests.
export function rungForPressure(stalls) {
  if (stalls >= 15) return 4;
  if (stalls >= 10) return 3;
  if (stalls >= 5) return 2;
  if (stalls >= 2) return 1;
  return 0;
}
export function pressureSeries(stalls, optimized = false) {
  return [0.15,0.2,0.16,0.25,0.3,0.5,0.7,0.85,1,0.95,0.9,1].map((v,i) => Math.max(0.05,stalls*v*(optimized && i>6 ? 0.2 : 1)));
}
const steps = [
  ['Observe','Healthy memory can be full. Leave your work alone.'],
  ['Reclaim','Ask the OS for cold, reclaimable memory first.'],
  ['Page out','Move eligible cold pages out of active RAM where the platform supports it.'],
  ['Unload idle tabs','Keep eligible tabs open; their contents reload when you return.'],
  ['Pause watchlisted apps','Only eligible apps you allow can be paused. Closing an app always starts with a countdown you can cancel.'],
];
function draw(root) {
  const slider=root.querySelector('[data-pressure]');
  const stalls=Number(slider.value);
  const optimized=root.dataset.optimized==='true';
  const level=rungForPressure(stalls);
  root.querySelector('[data-pressure-value]').textContent=`${stalls}%`;
  root.querySelector('[data-rung]').textContent=optimized?'Pressure easing':steps[level][0];
  root.querySelector('[data-explanation]').textContent=optimized?'In this simulated recovery, pressure falls after the first eligible interventions. Real results depend on the workload.':steps[level][1];
  const series=pressureSeries(stalls,optimized);
  const path=series.map((v,i)=>`${i?'L':'M'}${20+i*28},${140-v/20*116}`).join(' ');
  root.querySelector('[data-chart-line]').setAttribute('d',path);
  root.querySelector('[data-chart-area]').setAttribute('d',`${path} L328,140 L20,140 Z`);
  root.querySelector('[data-chart-summary]').textContent=`Illustrative memory-stall chart: selected pressure ${stalls} percent; ${optimized?'simulated recovery shown':'no intervention simulated'}.`;
  for(const [i,node] of [...document.querySelectorAll('[data-step]')].entries()) {
    node.classList.toggle('current',i===level&&!optimized);
    node.classList.toggle('passed',i<level||optimized);
    node.setAttribute('aria-current',i===level&&!optimized?'step':'false');
  }
  const button=root.querySelector('[data-simulate]');
  button.disabled=stalls<2;
  button.textContent=optimized?'Reset example':'Simulate an intervention';
}
if(typeof document!=='undefined') {
  const root=document.querySelector('[data-memory-lab]');
  if(root) {
    root.querySelector('[data-pressure]').addEventListener('input',()=>{root.dataset.optimized='false';draw(root)});
    root.querySelector('[data-simulate]').addEventListener('click',()=>{root.dataset.optimized=root.dataset.optimized==='true'?'false':'true';draw(root)});
    draw(root);
  }
}
