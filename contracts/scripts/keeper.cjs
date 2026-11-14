// Charge every subscription that is due. Run it every few minutes (cron, a
// systemd timer or the host's scheduler):
//
//   KEEPER_KEY=0x... CHECKOUT=0x<contract> FROM_BLOCK=<deploy block> \
//     npx hardhat run scripts/keeper.cjs --network baseSepolia
//
// The keeper wallet pays gas only; `charge` moves the subscriber's USDC to the
// treasury and can't send anything to the caller, so this key needs no more
// trust than "may spend a little ETH on gas". Each account is simulated first,
// so a subscriber whose balance or allowance is short costs no gas: the
// failure is logged and the API shows the account in grace.
const { ethers } = require("hardhat");

async function dueAccounts(checkout, fromBlock) {
  const latest = await ethers.provider.getBlockNumber();
  const accounts = new Set();
  const step = 2000; // public RPCs cap eth_getLogs ranges
  for (let from = fromBlock; from <= latest; from += step) {
    const to = Math.min(from + step - 1, latest);
    for (const log of await checkout.queryFilter(checkout.filters.TrialStarted(), from, to)) {
      accounts.add(log.args.account);
    }
  }
  const due = [];
  for (const a of accounts) if (await checkout.isDue(a)) due.push(a);
  return due;
}

async function main() {
  const address = process.env.CHECKOUT;
  if (!address) throw new Error("set CHECKOUT to the contract address");
  const [keeper] = await ethers.getSigners();
  const checkout = (await ethers.getContractAt("RamWardenCheckout", address)).connect(keeper);
  const due = await dueAccounts(checkout, Number(process.env.FROM_BLOCK || 0));
  let charged = 0;
  for (const account of due) {
    try {
      await checkout.charge.staticCall(account);
    } catch (e) {
      console.log(`not charged ${account}: ${e.shortMessage || e.message}`);
      continue;
    }
    const receipt = await (await checkout.charge(account)).wait();
    console.log(`charged ${account} in ${receipt.hash}`);
    charged++;
  }
  console.log(`${due.length} due, ${charged} charged`);
}

module.exports = { dueAccounts };

if (require.main === module) {
  main().catch((e) => {
    console.error(e);
    process.exitCode = 1;
  });
}
