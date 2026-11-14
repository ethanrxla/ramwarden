// Recorded, anonymized RAM samples plus a separately labeled pressure simulator.
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
    fetch('assets/memory-sample.json').then(response=>{if(!response.ok)throw new Error('Sample unavailable');return response.json();}).then(recording=>{
      const samples=recording.samples;
      if(!samples?.length)return;
      const last=samples[samples.length-1];
      root.querySelector('.lab-stats strong').replaceChildren(document.createTextNode(String(Math.round(last.percent))),Object.assign(document.createElement('small'),{textContent:'%'}));
      root.querySelector('.lab-stats div>span:last-child').textContent=`${(last.used_mb/1024).toFixed(1)} / ${(last.total_mb/1024).toFixed(1)} GB · recorded`;
      const bar=root.querySelector('.memory-bar');
      bar.setAttribute('aria-label',`Measured RAM: ${last.percent}% used; ${(100-last.percent).toFixed(1)}% available. No reclaimable breakdown measured.`);
      bar.children[0].style.width=`${last.percent}%`;bar.children[1].style.width='0%';bar.children[2].style.width=`${100-last.percent}%`;
      root.querySelector('.chart-legend').textContent='RAM in use / available · measured on a developer Mac';
      root.querySelector('.demo-label').textContent='RECORDED DEVICE SAMPLE';
      root.querySelector('.lab-footnote').textContent=`Measured ${new Date(recording.captured_at).toLocaleString()}: ${samples.length} samples, ${recording.interval_seconds}s apart. Only aggregate RAM totals are shared. Pressure and intervention controls below are simulations, not measured savings.`;
      const path=samples.map((row,i)=>`${i?'L':'M'}${20+i*308/(samples.length-1)},${140-row.percent/100*116}`).join(' ');
      root.querySelector('[data-chart-line]').setAttribute('d',path);
      root.querySelector('[data-chart-area]').setAttribute('d',`${path} L328,140 L20,140 Z`);
      root.querySelector('.chart-title').textContent='RECORDED RAM IN USE · % · 0–100';
      root.querySelector('[data-chart-summary]').textContent=`Recorded memory use ranged from ${Math.min(...samples.map(s=>s.percent))}% to ${Math.max(...samples.map(s=>s.percent))}%. No cleanup was triggered for this sample.`;
      const markSimulation=()=>{root.querySelector('.chart-title').textContent='SIMULATED MEMORY STALLS · %';root.querySelector('.demo-label').textContent='PRESSURE SIMULATOR';};
      root.querySelector('[data-pressure]').addEventListener('input',markSimulation);
      root.querySelector('[data-simulate]').addEventListener('click',markSimulation);
    }).catch(()=>{});
  }
}
