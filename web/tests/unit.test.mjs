// Unit tests for the site's logic. Run with `node --test web/tests/`.
// No network, no browser, no wallet.

import assert from "node:assert/strict";
import test from "node:test";

import { Api, ApiError, normaliseUserCode } from "../assets/api.js";
import { base58, utf8ToHex, walletErrorMessage } from "../assets/wallet.js";
import { detectOs } from "../assets/home.js";

test("user codes are normalised to the server's alphabet", () => {
  assert.equal(normaliseUserCode("bcdf-ghjk"), "BCDF-GHJK");
  assert.equal(normaliseUserCode(" BCDF GHJK "), "BCDF-GHJK");
  assert.equal(normaliseUserCode("BCDFGHJK"), "BCDF-GHJK");
  // Vowels and digits are never in a code, so a typo is refused, not guessed.
  assert.equal(normaliseUserCode("ABCD-EFGH"), null);
  assert.equal(normaliseUserCode("BCDF-GHJ0"), null);
  assert.equal(normaliseUserCode("BCDF-GHJ"), null);
  assert.equal(normaliseUserCode(null), null);
});

test("base58 matches the Bitcoin/Solana encoding", () => {
  assert.equal(base58(new Uint8Array([])), "");
  assert.equal(base58(new Uint8Array([0])), "1");
  assert.equal(base58(new Uint8Array([0, 0, 1])), "112");
  assert.equal(base58(new TextEncoder().encode("hello world")), "StV1DL6CwTryKyV");
  assert.equal(base58(new Uint8Array(32)), "1".repeat(32)); // the system program id
});

test("messages are hex-encoded as UTF-8 for personal_sign", () => {
  assert.equal(utf8ToHex("hi"), "0x6869");
  assert.equal(utf8ToHex("é"), "0xc3a9");
});

test("a wallet rejection reads as a cancel, not an error", () => {
  assert.match(walletErrorMessage({ code: 4001, message: "User rejected the request." }), /cancelled/);
  assert.match(walletErrorMessage(new Error("User rejected the request.")), /cancelled/);
  assert.match(walletErrorMessage({ code: -32002 }), /already has a request/);
});

test("the OS guess covers the three platforms", () => {
  assert.equal(detectOs({ userAgentData: { platform: "Windows" } }), "windows");
  assert.equal(detectOs({ platform: "MacIntel" }), "macos");
  assert.equal(detectOs({ platform: "Linux x86_64" }), "linux");
  assert.equal(detectOs({ platform: "" , userAgent: "" }), null);
});

class MemoryStorage {
  constructor() { this.m = new Map(); }
  getItem(k) { return this.m.has(k) ? this.m.get(k) : null; }
  setItem(k, v) { this.m.set(k, String(v)); }
  removeItem(k) { this.m.delete(k); }
}

function fakeFetch(routes) {
  const calls = [];
  const f = async (url, init) => {
    calls.push({ url, init });
    const r = routes[`${init.method} ${new URL(url).pathname}`];
    if (!r) return new Response(JSON.stringify({ error: "not found" }), { status: 404 });
    return new Response(JSON.stringify(r.body), { status: r.status ?? 200 });
  };
  f.calls = calls;
  return f;
}

test("verify stores the session and later calls send it", async () => {
  const fetchImpl = fakeFetch({
    "POST /v1/auth/verify": { body: { session: "s1", account: { id: "a" } } },
    "GET /v1/account": { body: { id: "a" } },
  });
  const api = new Api("https://api.example/", { fetchImpl, storage: new MemoryStorage() });
  await api.verify({ chain: "evm", address: "0xabc", message: "m", signature: "0x1" });
  await api.account();
  const last = fetchImpl.calls.at(-1);
  assert.equal(last.url, "https://api.example/v1/account");
  assert.equal(last.init.headers.Authorization, "Bearer s1");
  assert.equal(last.init.credentials, "omit");
});

test("a 401 clears the session; an unreachable server is reported as offline", async () => {
  const storage = new MemoryStorage();
  storage.setItem("rw.session", "old");
  const api = new Api("https://api.example", { fetchImpl: fakeFetch({ "GET /v1/account": { status: 401, body: { error: "expired" } } }), storage });
  await assert.rejects(api.account(), (e) => e instanceof ApiError && e.status === 401 && e.message === "expired");
  assert.equal(api.session, null);

  const down = new Api("https://api.example", { fetchImpl: async () => { throw new TypeError("Failed to fetch"); }, storage });
  await assert.rejects(down.checkoutConfig(), (e) => e.offline);
});

test("challenge sends a null user code for a plain web sign-in", async () => {
  const fetchImpl = fakeFetch({ "POST /v1/auth/challenge": { body: { message: "m" } } });
  const api = new Api("https://api.example", { fetchImpl, storage: new MemoryStorage() });
  await api.challenge({ chain: "solana", address: "So1" });
  assert.deepEqual(JSON.parse(fetchImpl.calls[0].init.body), { chain: "solana", address: "So1", user_code: null });
});
