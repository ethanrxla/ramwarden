// Client for the hosted API's web routes. The contract is in
// docs/specs/website.md; this file and that spec change together.
//
// Why the server builds every message and transaction:
//
// - A SIWE message must carry the EIP-55 checksummed address, and Base
//   calldata needs keccak-256 function selectors. Doing either here means
//   shipping a hashing library to the browser and keeping two copies of the
//   contract ABI in step. The server already has both.
// - The page cannot be trusted to word the message. If the server builds it,
//   the nonce, domain, user code and expiry are exactly what it will verify.
//
// So the page asks for a message, has the wallet sign it as shown, and sends
// the signature back. The wallet still displays the full text to the user
// before signing, which is the check that matters.

export class ApiError extends Error {
  constructor(status, message) {
    super(message);
    this.status = status;
  }
  // No answer at all (DNS, CORS, server down), as opposed to a refusal.
  get offline() {
    return this.status === 0;
  }
}

const SESSION_KEY = "rw.session";

export class Api {
  constructor(base, { fetchImpl = globalThis.fetch?.bind(globalThis), storage = globalThis.sessionStorage } = {}) {
    this.base = base.replace(/\/+$/, "");
    this.fetch = fetchImpl;
    this.storage = storage;
  }

  // The web session is per tab: it lives in sessionStorage, so closing the tab
  // signs out, and it never sits on disk for another site or user to find.
  get session() {
    return this.storage?.getItem(SESSION_KEY) || null;
  }
  set session(token) {
    if (!this.storage) return;
    if (token) this.storage.setItem(SESSION_KEY, token);
    else this.storage.removeItem(SESSION_KEY);
  }

  async request(method, path, body) {
    const headers = { Accept: "application/json" };
    if (body !== undefined) headers["Content-Type"] = "application/json";
    if (this.session) headers.Authorization = `Bearer ${this.session}`;
    let resp;
    try {
      resp = await this.fetch(this.base + path, {
        method,
        headers,
        body: body === undefined ? undefined : JSON.stringify(body),
        credentials: "omit",
      });
    } catch {
      throw new ApiError(0, "The RamWarden server could not be reached.");
    }
    let data = null;
    try {
      data = await resp.json();
    } catch {
      // An empty or non-JSON body is fine for errors; the status says enough.
    }
    if (!resp.ok) {
      if (resp.status === 401) this.session = null;
      throw new ApiError(resp.status, (data && data.error) || `Request failed (${resp.status}).`);
    }
    return data;
  }

  // Device sign-in (RFC 8628). Shows which machine asked, so a user who was
  // sent someone else's code can see it is not theirs before signing.
  lookupDevice(userCode) {
    return this.request("GET", `/v1/device/lookup?user_code=${encodeURIComponent(userCode)}`);
  }

  // `userCode` is set when signing in to link a device, and omitted for a
  // plain website sign-in (account page, checkout).
  challenge({ chain, address, userCode }) {
    return this.request("POST", "/v1/auth/challenge", { chain, address, user_code: userCode ?? null });
  }

  async verify({ chain, address, message, signature }) {
    const r = await this.request("POST", "/v1/auth/verify", { chain, address, message, signature });
    this.session = r.session;
    return r;
  }

  account() {
    return this.request("GET", "/v1/account");
  }
  savePrefs(prefs) {
    return this.request("PATCH", "/v1/account/prefs", prefs);
  }
  // A personal install line for a new computer: single use, 30 minutes.
  installLink() {
    return this.request("POST", "/v1/install/link", {});
  }
  revokeDevice(id) {
    return this.request("DELETE", `/v1/account/devices/${encodeURIComponent(id)}`);
  }
  signOut() {
    this.session = null;
  }

  checkoutConfig() {
    return this.request("GET", "/v1/checkout/config");
  }
  checkoutBase(plan, payer) {
    return this.request("POST", "/v1/checkout/base", { plan, payer });
  }
  checkoutSolana(plan, payer) {
    return this.request("POST", "/v1/checkout/solana", { plan, payer });
  }
  checkoutStripe(plan) {
    return this.request("POST", "/v1/checkout/stripe", { plan });
  }
  payment(id) {
    return this.request("GET", `/v1/checkout/payments/${encodeURIComponent(id)}`);
  }
}

// The server's alphabet (`ramwarden-cloud` `user_code()`): consonants only, as
// RFC 8628 suggests, so a code cannot spell a word or be misread as a digit.
export const USER_CODE_ALPHABET = "BCDFGHJKLMNPQRSTVWXZ";

// Normalises what a user types into XXXX-XXXX. Spaces and dashes are dropped;
// any other character outside the alphabet makes the code invalid rather than
// being guessed at.
export function normaliseUserCode(raw) {
  const s = String(raw || "").toUpperCase().replace(/[\s-]/g, "");
  if (s.length !== 8 || [...s].some((c) => !USER_CODE_ALPHABET.includes(c))) return null;
  return `${s.slice(0, 4)}-${s.slice(4)}`;
}
