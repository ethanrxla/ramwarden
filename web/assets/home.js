// Both warden tabs: the rotating catchline, the hero clip, the install box
// and pricing (shared components), and the pets.

import { makeApi, mockBanner } from "./common.js";
import { InstallBox, detectOs } from "./install.js";
import { PetColor, PetStage, WardenPet, loadLines } from "./pets.js";
import { Pricing } from "./pricing.js";

export { detectOs };

const reduceMotion = () => matchMedia("(prefers-reduced-motion: reduce)").matches;

// The italic catchline cycles through its lines, one every few seconds.
// "Your PC" reads "Your Mac" for Mac visitors. Reduced motion keeps the first.
export function setUpRotators(root = document, os = detectOs()) {
  for (const el of root.querySelectorAll("[data-rotate]")) {
    let lines;
    try {
      lines = JSON.parse(el.dataset.rotate);
    } catch {
      continue;
    }
    if (os === "macos") lines = lines.map((l) => l.replace(/\bYour PC\b/g, "Your Mac"));
    if (reduceMotion()) continue;
    const em = el.querySelector("em") || el;
    let i = Math.max(0, lines.indexOf(em.textContent));
    setInterval(() => {
      em.classList.add("out");
      setTimeout(() => {
        i = (i + 1) % lines.length;
        em.textContent = lines[i];
        em.classList.remove("out");
      }, 350);
    }, 3200);
  }
}

// The hero clip loops muted. Reduced motion, or a phone on a saver, keeps the
// poster.
function setUpHeroClip() {
  const v = document.querySelector("[data-hero-clip]");
  if (!v || reduceMotion()) return;
  const phone = matchMedia("(max-width: 860px)").matches;
  v.src = (phone && v.dataset.srcMobile) || v.dataset.src;
  v.play().catch(() => {});
}

// Signed in? Only the account's own status is read, to pick the install
// box's face. No server, or no session: the signed-out face.
async function whoIsHere(api) {
  if (!api.session) return null;
  try {
    return await api.account();
  } catch {
    return null;
  }
}

async function main() {
  mockBanner();
  setUpRotators();
  setUpHeroClip();

  const pricing = document.querySelector("[data-pricing]");
  if (pricing) new Pricing(pricing).render();

  const api = await makeApi();
  const account = await whoIsHere(api);
  const install = document.querySelector("[data-install]");
  if (install) await new InstallBox(install, { api, account }).render();

  const warden = document.body.dataset.warden || "ram";
  const lines = await loadLines(warden);
  const color = new PetColor(warden, `assets/pets/${warden}.svg`);
  if (account && account.prefs?.wardens?.[warden]?.color != null && color.hue == null) color.hue = account.prefs.wardens[warden].color;
  await color.load();
  color.addEventListener("change", (e) => {
    // Signed in, the color follows the account to the apps.
    if (account) api.savePrefs({ wardens: { [warden]: { color: e.detail.hue } } }).catch(() => {});
  });
  const stage = document.querySelector("[data-stage]");
  if (stage) new PetStage(stage, lines, color).start();
  const pet = document.querySelector("[data-pet]");
  if (pet) new WardenPet(pet, lines).start();
}

if (typeof document !== "undefined") main();
