// Deploy RamWardenCheckout and write the plans in plans.json.
//
//   DEPLOYER_KEY=0x... TREASURY=0x<Safe> OWNER=0x<Safe> \
//     npx hardhat run scripts/deploy.cjs --network baseSepolia
//
// OWNER defaults to the deployer so the plans can be written in the same run;
// hand ownership to the Safe afterwards with transferOwnership + acceptOwnership
// (two steps, so a typo cannot lose the contract).
const { ethers, network } = require("hardhat");
const plans = require("../plans.json");

// Circle's USDC. Check against https://developers.circle.com/stablecoins/usdc-contract-addresses before mainnet.
const USDC = {
  base: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913",
  baseSepolia: "0x036CbD53842c5426634e7929541eC2318f3dCF7e",
};

async function main() {
  const [deployer] = await ethers.getSigners();
  const token = process.env.USDC || USDC[network.name];
  const treasury = process.env.TREASURY;
  if (!token || !treasury) throw new Error("set TREASURY (and USDC on a network without a default)");
  const owner = process.env.OWNER || deployer.address;

  const checkout = await (await ethers.getContractFactory("RamWardenCheckout")).deploy(token, treasury, deployer.address);
  await checkout.waitForDeployment();
  const address = await checkout.getAddress();
  console.log(`RamWardenCheckout ${address} on ${network.name}, token ${token}, treasury ${treasury}`);

  for (const p of plans.plans) {
    const mask = p.wardens.reduce((m, w) => m | (1 << w), 0);
    const tx = await checkout.setPlan(p.id, BigInt(p.usd) * 1_000_000n, p.days * 86_400, mask, p.trial === true, true);
    await tx.wait();
    console.log(`  plan ${p.id}: ${p.name}, ${p.usd} USDC, wardens ${p.wardens.join("+")}${p.trial ? ", 14-day trial" : ""}`);
  }

  if (owner.toLowerCase() !== deployer.address.toLowerCase()) {
    await (await checkout.transferOwnership(owner)).wait();
    console.log(`  ownership offered to ${owner}; accept it from that account`);
  }
}

main().catch((e) => {
  console.error(e);
  process.exitCode = 1;
});
