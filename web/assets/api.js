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

// The session is an HttpOnly cookie (`rw_session`, Secure, SameSite=Strict)
// that the sign-in routes set. The page never sees the token, so a script
// injected into it could not steal one; every call just sends the cookie.

export class Api {
  constructor(base, { fetchImpl = globalThis.fetch?.bind(globalThis) } = {}) {
    this.base = base.replace(/\/+$/, "");
    this.fetch = fetchImpl;
  }

  async request(method, path, body) {
    const headers = { Accept: "application/json" };
    if (body !== undefined) headers["Content-Type"] = "application/json";
    let resp;
    try {
      resp = await this.fetch(this.base + path, {
        method,
        headers,
        body: body === undefined ? undefined : JSON.stringify(body),
        credentials: "include",
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
    if (!resp.ok) throw new ApiError(resp.status, (data && data.error) || `Request failed (${resp.status}).`);
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

  // Both sign-ins answer `{account, linked_device}` and set the cookie.
  // 409 means the account is at its device limit (Lite 1, Plus 3, Pro 5).
  verify({ chain, address, message, signature }) {
    return this.request("POST", "/v1/auth/verify", { chain, address, message, signature });
  }
  emailStart({ email, userCode }) {
    return this.request("POST", "/v1/auth/email/start", { email, user_code: userCode ?? null });
  }
  emailVerify({ email, code }) {
    return this.request("POST", "/v1/auth/email/verify", { email, code });
  }
  logout() {
    return this.request("POST", "/v1/auth/logout");
  }

  account() {
    return this.request("GET", "/v1/account");
  }
  // Replaces the whole prefs object (4 KB at most), so send all of it.
  savePrefs(prefs) {
    return this.request("PUT", "/v1/account/prefs", prefs);
  }
  // A personal install line for a new computer: single use, 30 minutes.
  installLink() {
    return this.request("POST", "/v1/install/link", {});
  }
  revokeDevice(id) {
    return this.request("DELETE", `/v1/account/devices/${encodeURIComponent(id)}`);
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
  // Base: transactions for the paying wallet to send. Card: done server side.
  cancel() {
    return this.request("POST", "/v1/checkout/cancel");
  }
  payment(id) {
    return this.request("GET", `/v1/checkout/payments/${encodeURIComponent(id)}`);
  }

  // The site wardens' support chat (docs/specs/website.md, "Support chat").
  // `conversation` is the secret the first answer hands back; the page keeps
  // it in localStorage so the thread, and any reply from the team, is there
  // next visit.
  supportHistory({ conversation, warden }) {
    return this.request("POST", "/v1/support/history", { conversation: conversation ?? null, warden });
  }
  // Either `message` (typed) or `topic` (a chip's topic id).
  supportChat({ conversation, warden, message, topic }) {
    return this.request("POST", "/v1/support/chat", { conversation: conversation ?? null, warden, message: message ?? null, topic: topic ?? null });
  }
  supportTicket({ conversation, email }) {
    return this.request("POST", "/v1/support/ticket", { conversation, email: email || null });
  }
  supportTickets() {
    return this.request("GET", "/v1/support/tickets");
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
