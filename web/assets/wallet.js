// Browser wallets: MetaMask (and any EIP-1193 wallet) for Base, Phantom for
// Solana. Each wallet is a small class with the same three verbs, connect,
// sign and pay, so the pages never branch on which wallet is in use.
//
// No wallet library is loaded. The two providers' own APIs cover everything
// here, and a page that asks people to sign should run as little third-party
// code as possible.

// Finds EVM wallets the EIP-6963 way, so MetaMask is found even when Phantom
// or Coinbase Wallet has claimed `window.ethereum`. Falls back to
// `window.ethereum` for wallets that predate the standard.
export function discoverEvmWallets(win = globalThis, waitMs = 150) {
  return new Promise((resolve) => {
    const found = new Map();
    const onAnnounce = (e) => {
      const { info, provider } = e.detail || {};
      if (info?.uuid && provider) found.set(info.uuid, { name: info.name, rdns: info.rdns, icon: info.icon, provider });
    };
    win.addEventListener?.("eip6963:announceProvider", onAnnounce);
    win.dispatchEvent?.(new Event("eip6963:requestProvider"));
    setTimeout(() => {
      win.removeEventListener?.("eip6963:announceProvider", onAnnounce);
      const list = [...found.values()];
      if (list.length === 0 && win.ethereum) {
        list.push({ name: win.ethereum.isMetaMask ? "MetaMask" : "Browser wallet", rdns: null, icon: null, provider: win.ethereum });
      }
      // MetaMask first: it is the one the pricing page names.
      list.sort((a, b) => (b.rdns === "io.metamask") - (a.rdns === "io.metamask"));
      resolve(list);
    }, waitMs);
  });
}

export function phantomProvider(win = globalThis) {
  const p = win.phantom?.solana || win.solana;
  return p && p.isPhantom ? p : null;
}

export function utf8ToHex(text) {
  return "0x" + [...new TextEncoder().encode(text)].map((b) => b.toString(16).padStart(2, "0")).join("");
}

const B58 = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

// Solana signatures and messages travel as base58, the encoding every Solana
// tool and explorer uses.
export function base58(bytes) {
  const digits = []; // base-58 digits, least significant first
  for (const byte of bytes) {
    let carry = byte;
    for (let i = 0; i < digits.length; i++) {
      carry += digits[i] << 8;
      digits[i] = carry % 58;
      carry = (carry / 58) | 0;
    }
    while (carry) {
      digits.push(carry % 58);
      carry = (carry / 58) | 0;
    }
  }
  let out = "";
  // Each leading zero byte is a leading "1", as in Bitcoin's encoding.
  for (const byte of bytes) {
    if (byte !== 0) break;
    out += "1";
  }
  for (let i = digits.length - 1; i >= 0; i--) out += B58[digits[i]];
  return out;
}

// What a user sees when a wallet says no. Rejections are normal and should
// read as such, not as an error.
export function walletErrorMessage(err) {
  const code = err?.code;
  if (code === 4001 || /reject|denied|cancel/i.test(err?.message || "")) return "You cancelled in your wallet. Nothing was signed or sent.";
  if (code === -32002) return "Your wallet already has a request open. Check its window.";
  return err?.message || "The wallet returned an error.";
}

export class EvmWallet {
  constructor({ name, provider }) {
    this.chain = "evm";
    this.name = name;
    this.provider = provider;
    this.address = null;
  }

  async connect() {
    const [address] = await this.provider.request({ method: "eth_requestAccounts" });
    if (!address) throw new Error("The wallet did not share an account.");
    this.address = address;
    return address;
  }

  // EIP-191 personal_sign: the wallet shows the text, signs it, and no gas or
  // funds are involved.
  sign(message) {
    return this.provider.request({ method: "personal_sign", params: [utf8ToHex(message), this.address] });
  }

  async ensureChain(chainId) {
    const hex = "0x" + Number(chainId).toString(16);
    const current = await this.provider.request({ method: "eth_chainId" });
    if (String(current).toLowerCase() === hex) return;
    try {
      await this.provider.request({ method: "wallet_switchEthereumChain", params: [{ chainId: hex }] });
    } catch (err) {
      // 4902: the wallet does not know the chain yet.
      if (err?.code !== 4902 || !BASE_CHAINS[chainId]) throw err;
      await this.provider.request({ method: "wallet_addEthereumChain", params: [BASE_CHAINS[chainId]] });
    }
  }

  // Sends each server-built transaction in order (a USDC approval, then the
  // purchase) and returns the hashes. The user confirms each in the wallet.
  async pay({ chain_id, transactions }) {
    await this.ensureChain(chain_id);
    const hashes = [];
    for (const tx of transactions) {
      hashes.push(
        await this.provider.request({
          method: "eth_sendTransaction",
          params: [{ from: this.address, to: tx.to, data: tx.data, value: tx.value || "0x0" }],
        }),
      );
    }
    return hashes;
  }
}

export class PhantomWallet {
  constructor(provider) {
    this.chain = "solana";
    this.name = "Phantom";
    this.provider = provider;
    this.address = null;
  }

  async connect() {
    const r = await this.provider.connect();
    this.address = r.publicKey.toString();
    return this.address;
  }

  async sign(message) {
    const { signature } = await this.provider.signMessage(new TextEncoder().encode(message), "utf8");
    return base58(signature);
  }

  // The server builds the USDC transfer (with its Solana Pay reference key)
  // and sends the serialized message as base58; Phantom signs and submits it.
  async pay({ message }) {
    const r = await this.provider.request({ method: "signAndSendTransaction", params: { message } });
    return [r.signature];
  }
}

const BASE_CHAINS = {
  8453: {
    chainId: "0x2105",
    chainName: "Base",
    nativeCurrency: { name: "Ether", symbol: "ETH", decimals: 18 },
    rpcUrls: ["https://mainnet.base.org"],
    blockExplorerUrls: ["https://basescan.org"],
  },
  84532: {
    chainId: "0x14a34",
    chainName: "Base Sepolia",
    nativeCurrency: { name: "Ether", symbol: "ETH", decimals: 18 },
    rpcUrls: ["https://sepolia.base.org"],
    blockExplorerUrls: ["https://sepolia.basescan.org"],
  },
};
