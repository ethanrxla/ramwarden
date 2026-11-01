// Both warden tabs: the rotating catchline, the install box
// and pricing (shared components), and the pets.

import { makeApi, mockBanner } from "./common.js";
import { InstallBox, detectOs } from "./install.js";
import { PetLook, PetStage, WardenPet, loadLines } from "./pets.js";
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

// Signed in? The session is an HttpOnly cookie, so the page asks. Only the
// account's own status is read, to pick the install box's face. No server,
// or a 401: the signed-out face.
async function whoIsHere(api) {
  try {
    return await api.account();
  } catch {
    return null;
  }
}

async function main() {
  mockBanner();
  setUpRotators();

  const pricing = document.querySelector("[data-pricing]");
  if (pricing) new Pricing(pricing).render();

  const api = await makeApi();
  const account = await whoIsHere(api);
  const install = document.querySelector("[data-install]");
  if (install) await new InstallBox(install, { api, account }).render();

  const warden = document.body.dataset.warden || "ram";
  const lines = await loadLines(warden);
  const look = new PetLook(warden);
  look.adopt(account?.prefs?.wardens?.[warden]?.look);
  look.paint();
  look.addEventListener("change", (e) => {
    // Signed in, the look follows the account to the apps.
    // Prefs are replaced whole, so the rest of them go back unchanged.
    if (!account) return;
    const prefs = { ...(account.prefs || {}) };
    prefs.wardens = { ...(prefs.wardens || {}), [warden]: { ...(prefs.wardens?.[warden] || {}), look: e.detail.look } };
    account.prefs = prefs;
    api.savePrefs(prefs).catch(() => {});
  });
  const stage = document.querySelector("[data-stage]");
  if (stage) new PetStage(stage, lines, look).start();
  const pet = document.querySelector("[data-pet]");
  if (pet) new WardenPet(pet, lines).start();
}

if (typeof document !== "undefined") main();
