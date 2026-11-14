// Decorative city motion never competes with an inactive page or reduced motion.
const scenes = [...document.querySelectorAll('[data-city-scene]')];
const motion = matchMedia('(prefers-reduced-motion: reduce)');
let paused = false;
let scheduled = false;
const visible = new Set();
function playback() {
  for (const scene of scenes) {
    const video = scene.querySelector('[data-city-video]');
    if (!video) continue;
    const play = visible.has(scene) && !document.hidden && !motion.matches && !paused;
    if (play) {
      if (!video.getAttribute('src')) video.src = video.dataset.cityVideo;
      video.play().catch(() => {}); // Poster remains useful if autoplay is blocked.
    } else video.pause();
  }
}
function paint() {
  scheduled = false;
  for (const scene of visible) {
    const r = scene.getBoundingClientRect();
    const p = Math.max(0, Math.min(1, (innerHeight - r.top) / (innerHeight + r.height)));
    scene.style.setProperty('--city-shift', motion.matches || paused ? '0px' : `${(p - .5) * 90}px`);
    scene.style.setProperty('--city-storm', motion.matches || paused ? '0' : `${Math.max(0, (p - .3) * .8)}`);
  }
}
function schedule() {
  if (!scheduled) { scheduled = true; requestAnimationFrame(paint); }
}
const observer = new IntersectionObserver(entries => {
  for (const entry of entries) {
    if (entry.isIntersecting) visible.add(entry.target);
    else visible.delete(entry.target);
  }
  playback(); schedule();
});
scenes.forEach(scene => observer.observe(scene));
addEventListener('scroll', schedule, { passive: true });
addEventListener('resize', schedule);
document.addEventListener('visibilitychange', playback);
motion.addEventListener('change', () => { playback(); schedule(); });
for (const button of document.querySelectorAll('.city-motion')) {
  button.addEventListener('click', () => {
    paused = !paused;
    button.setAttribute('aria-pressed', String(paused));
    button.textContent = paused ? 'Play background' : 'Pause background';
    playback(); schedule();
  });
}

// Repeatable reveals: scrolling away resets each block for the next visit.
if (!motion.matches) {
  const reveal = new IntersectionObserver(entries => {
    for (const {target,isIntersecting} of entries) target.classList.toggle('in-view',isIntersecting);
  }, {threshold:.08});
  document.querySelectorAll('.landing .block > .wrap, .city-caption').forEach(el => {
    el.classList.add('scroll-reveal'); reveal.observe(el);
  });
}
