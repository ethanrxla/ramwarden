// Site rules from docs/product-spec.md 2: copy rules, the pet's look and
// pricing math.

import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import { annualSaving, config, planFor } from "../assets/config.js";
import { presetLook } from "../assets/pets.js";
import { DEFAULT_LOOKS, normalizeLook, spriteSvg, tint } from "../assets/sprite.js";
import { installState } from "../assets/install.js";

const WEB = path.join(path.dirname(fileURLToPath(import.meta.url)), "..");
const TEXT = /\.(html|js|mjs|cjs|json|css|md|sh|ps1|svg)$/;

function files(dir) {
  return fs.readdirSync(dir, { withFileTypes: true }).flatMap((e) => {
    const p = path.join(dir, e.name);
    return e.isDirectory() ? files(p) : TEXT.test(e.name) ? [p] : [];
  });
}

test("no em dashes anywhere under web/", () => {
  const hits = files(WEB).filter((f) => fs.readFileSync(f, "utf8").includes(String.fromCharCode(0x2014)));
  assert.deepEqual(hits.map((f) => path.relative(WEB, f)), []);
});

test("pages never name the engines or providers, or say 'our server'", () => {
  const banned = /\b(wazuh|clamav|suricata|ntopng|jev|typesafe|mongodb|oracle)\b|our servers?|ramwarden's servers/i;
  const pages = fs.readdirSync(WEB).filter((f) => f.endsWith(".html") && f !== "notices.html");
  const json = fs.readdirSync(path.join(WEB, "assets/pets")).filter((f) => f.endsWith(".json")).map((f) => "assets/pets/" + f);
  const hits = [...pages, ...json].filter((f) => banned.test(fs.readFileSync(path.join(WEB, f), "utf8")));
  assert.deepEqual(hits, []);
});

test("no command from raw.githubusercontent.com on the pages", () => {
  for (const f of fs.readdirSync(WEB).filter((f) => f.endsWith(".html"))) {
    assert.doesNotMatch(fs.readFileSync(path.join(WEB, f), "utf8"), /raw\.githubusercontent\.com/, f);
  }
});

test("both warden tabs have the same section skeleton", () => {
  const skeleton = (f) => [...fs.readFileSync(path.join(WEB, f), "utf8").matchAll(/data-section="([a-z]+)"/g)].map((m) => m[1]);
  const ram = skeleton("index.html");
  assert.deepEqual(ram, ["hero", "install", "world", "pet", "how", "pricing", "faq"]);
  assert.deepEqual(skeleton("threat.html"), ram);
});

test("pet lines cover every section of both tabs", () => {
  for (const w of ["ram", "threat"]) {
    const lines = JSON.parse(fs.readFileSync(path.join(WEB, `assets/pets/${w}-lines.json`), "utf8"));
    assert.deepEqual(Object.keys(lines.sections).sort(), ["faq", "get", "how", "meet", "pricing", "top", "world"]);
    assert.ok(lines.jokes.length >= 6);
  }
});

// The drawn sprites must be the shipped ones until the visitor changes them,
// so the page never jumps between two different rams.
test("drawn sprites match the shipped SVGs in their original look", () => {
  for (const w of ["ram", "threat"]) {
    const rects = (svg) => svg.match(/<rect[^>]*>/g).map((r) => r.toLowerCase()).sort();
    const shipped = fs.readFileSync(path.join(WEB, `assets/pets/${w}.svg`), "utf8");
    assert.deepEqual(rects(spriteSvg(w, DEFAULT_LOOKS[w])), rects(shipped), w);
  }
});

test("RamWarden keeps its own shape: no antenna, no visor", () => {
  const ram = spriteSvg("ram", DEFAULT_LOOKS.ram);
  assert.doesNotMatch(ram, /#9aa3ad|#e3e8ee|#5b636d/, "no metal");
  assert.doesNotMatch(ram, /<rect x="17" y="0"/, "no antenna tip");
});

test("each part recolors with the app's tints, outline stays", () => {
  const svg = spriteSvg("ram", { ...DEFAULT_LOOKS.ram, wool: "#3fa2f2", horns: "#f5deaa", eyes: "#ffd000" });
  assert.match(svg, /fill="#3fa2f2"/);
  assert.match(svg, new RegExp(`fill="${tint("#3fa2f2", 0.62)}"`));
  assert.match(svg, new RegExp(`fill="${tint("#f5deaa", 0.66)}"`));
  assert.match(svg, /fill="#ffd000"/);
  assert.match(svg, /fill="#0d0b0e"/);
  assert.doesNotMatch(svg, /#f24455|#a8232f/);
  assert.equal(tint("#808080", 2), "#ffffff");
});

test("pupil styles change the eyes and bad input falls back", () => {
  const slit = spriteSvg("ram", { ...DEFAULT_LOOKS.ram, pupil: "slit" });
  const none = spriteSvg("ram", { ...DEFAULT_LOOKS.ram, pupil: "none" });
  const round = spriteSvg("ram", { ...DEFAULT_LOOKS.ram, pupil: "round" });
  assert.notEqual(slit, none);
  assert.notEqual(slit, round);
  assert.deepEqual(normalizeLook("ram", { wool: "red", pupil: "x", horns: "#ABCDEF" }), { ...DEFAULT_LOOKS.ram, horns: "#abcdef" });
  assert.equal(presetLook("ram", DEFAULT_LOOKS.ram, "#3fa2f2").horns, tint("#3fa2f2", 0.8));
});

test("plans match the product spec", () => {
  assert.deepEqual(
    config.plans.map((p) => [p.id, p.tier, p.interval, p.usd]),
    [[1, "ram", "month", 4], [2, "pro", "month", 5], [3, "ram", "year", 20], [4, "pro", "year", 24]],
  );
  assert.equal(planFor(config.plans, "pro", "year").id, 4);
  assert.equal(annualSaving(config.plans, "ram"), 58);
  assert.equal(annualSaving(config.plans, "pro"), 60);
  assert.equal(config.trialDays, 14);
});

test("three cards: Free, Plus with the only trial, Pro paid from day one", () => {
  assert.equal(config.lite.name, "RamWarden Lite");
  assert.equal(config.tiers.ram.name, "RamWarden Plus");
  assert.equal(config.tiers.ram.trialDays, 14);
  assert.equal(config.tiers.pro.trialDays, 0);
  for (const t of [config.lite, config.tiers.ram, config.tiers.pro]) assert.ok(t.features.length >= 5);
});

test("install box state follows the account", () => {
  const now = Date.parse("2026-11-01T00:00:00Z");
  assert.equal(installState(null, now), "signed-out");
  assert.equal(installState({ status: null }, now), "signed-out");
  assert.equal(installState({ status: "trial" }, now), "ready");
  assert.equal(installState({ status: "cancelled", paid_until: "2026-11-20T00:00:00Z" }, now), "ready");
  assert.equal(installState({ status: "cancelled", paid_until: "2026-10-20T00:00:00Z" }, now), "renew");
  assert.equal(installState({ status: "lapsed" }, now), "renew");
});
