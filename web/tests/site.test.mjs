// Site rules from docs/product-spec.md 2: copy rules, pet recoloring and
// pricing math.

import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import { annualSaving, config, planFor } from "../assets/config.js";
import { BODY_PALETTES, recolorSvg } from "../assets/pets.js";
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
  assert.deepEqual(ram, ["hero", "install", "world", "pet", "how", "pricing", "faq", "demo"]);
  assert.deepEqual(skeleton("threat.html"), ram);
});

test("pet lines cover every section of both tabs", () => {
  for (const w of ["ram", "threat"]) {
    const lines = JSON.parse(fs.readFileSync(path.join(WEB, `assets/pets/${w}-lines.json`), "utf8"));
    assert.deepEqual(Object.keys(lines.sections).sort(), ["demo", "faq", "get", "how", "meet", "pricing", "top", "world"]);
    assert.ok(lines.jokes.length >= 6);
  }
});

test("recolor moves only body colors and keeps outline and eyes", () => {
  const svg = '<rect fill="#f24455"/><rect fill="#0d0b0e"/><rect fill="#ffffff"/><rect fill="#a8232f"/>';
  const out = recolorSvg(svg, BODY_PALETTES.ram, 210);
  assert.match(out, /fill="#0d0b0e"/);
  assert.match(out, /fill="#ffffff"/);
  assert.doesNotMatch(out, /#f24455|#a8232f/);
  // Hue 354 is roughly the original crimson.
  const back = recolorSvg('<rect fill="#f24455"/>', BODY_PALETTES.ram, 354);
  assert.match(back, /fill="#f2445[0-9a-f]"/);
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

test("install box state follows the account", () => {
  const now = Date.parse("2026-11-01T00:00:00Z");
  assert.equal(installState(null, now), "signed-out");
  assert.equal(installState({ status: null }, now), "signed-out");
  assert.equal(installState({ status: "trial" }, now), "ready");
  assert.equal(installState({ status: "cancelled", paid_until: "2026-11-20T00:00:00Z" }, now), "ready");
  assert.equal(installState({ status: "cancelled", paid_until: "2026-10-20T00:00:00Z" }, now), "renew");
  assert.equal(installState({ status: "lapsed" }, now), "renew");
});
