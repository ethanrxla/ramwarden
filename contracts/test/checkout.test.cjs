const { expect } = require("chai");
const { ethers, network } = require("hardhat");

const DAY = 24 * 60 * 60;
const USDC = (n) => BigInt(Math.round(n * 1e6));
const RAM = 0; // the RAM warden's cloud tier
const WAZUH = 1; // the Wazuh warden add-on
const bit = (w) => 1 << w;
const ACCOUNT = ethers.id("account:65f0c0ffee");
const REF = ethers.id("payment:1");

async function now() {
  return (await ethers.provider.getBlock("latest")).timestamp;
}

async function forward(seconds) {
  await network.provider.send("evm_increaseTime", [seconds]);
  await network.provider.send("evm_mine");
}

async function deploy() {
  const [owner, treasury, buyer, other] = await ethers.getSigners();
  const usdc = await (await ethers.getContractFactory("MockUSDC")).deploy();
  const checkout = await (await ethers.getContractFactory("RamWardenCheckout")).deploy(
    await usdc.getAddress(),
    treasury.address,
    owner.address,
  );
  await checkout.setPlan(1, USDC(4), 30 * DAY, bit(RAM), true);
  await checkout.setPlan(3, USDC(6), 30 * DAY, bit(WAZUH), true);
  await checkout.setPlan(5, USDC(8), 30 * DAY, bit(RAM) | bit(WAZUH), true);
  await usdc.mint(buyer.address, USDC(1000));
  await usdc.connect(buyer).approve(await checkout.getAddress(), ethers.MaxUint256);
  return { owner, treasury, buyer, other, usdc, checkout };
}

describe("RamWardenCheckout", function () {
  it("moves the price straight to the treasury and extends only the plan's wardens", async function () {
    const { treasury, buyer, usdc, checkout } = await deploy();
    const tx = await checkout.connect(buyer).purchase(ACCOUNT, 1, USDC(4), REF);
    const t = await now();

    await expect(tx).to.emit(checkout, "Purchased").withArgs(ACCOUNT, 1, buyer.address, USDC(4), bit(RAM), REF);
    await expect(tx).to.emit(checkout, "Extended").withArgs(ACCOUNT, RAM, t + 30 * DAY);
    expect(await usdc.balanceOf(treasury.address)).to.equal(USDC(4));
    expect(await usdc.balanceOf(await checkout.getAddress())).to.equal(0n, "nothing rests in the contract");
    expect(await checkout.paidUntil(ACCOUNT, RAM)).to.equal(t + 30 * DAY);
    expect(await checkout.paidUntil(ACCOUNT, WAZUH)).to.equal(0n);
    expect(await checkout.isActive(ACCOUNT, RAM)).to.equal(true);
    expect(await checkout.isActive(ACCOUNT, WAZUH)).to.equal(false);
  });

  it("a bundle extends every warden it names", async function () {
    const { buyer, checkout } = await deploy();
    await checkout.connect(buyer).purchase(ACCOUNT, 5, USDC(8), REF);
    const t = await now();
    expect(await checkout.paidUntil(ACCOUNT, RAM)).to.equal(t + 30 * DAY);
    expect(await checkout.paidUntil(ACCOUNT, WAZUH)).to.equal(t + 30 * DAY);
  });

  it("renewing early stacks on the current expiry; renewing late starts from now", async function () {
    const { buyer, checkout } = await deploy();
    await checkout.connect(buyer).purchase(ACCOUNT, 1, USDC(4), REF);
    const first = await checkout.paidUntil(ACCOUNT, RAM);

    await forward(10 * DAY);
    await checkout.connect(buyer).purchase(ACCOUNT, 1, USDC(4), REF);
    expect(await checkout.paidUntil(ACCOUNT, RAM)).to.equal(first + BigInt(30 * DAY));

    await forward(100 * DAY);
    await checkout.connect(buyer).purchase(ACCOUNT, 1, USDC(4), REF);
    expect(await checkout.paidUntil(ACCOUNT, RAM)).to.equal(BigInt((await now()) + 30 * DAY));
  });

  it("anyone may pay for any account, so a pass can be a gift", async function () {
    const { buyer, other, usdc, checkout } = await deploy();
    await usdc.mint(other.address, USDC(10));
    await usdc.connect(other).approve(await checkout.getAddress(), USDC(4));
    await checkout.connect(other).purchase(ACCOUNT, 1, USDC(4), REF);
    expect(await checkout.isActive(ACCOUNT, RAM)).to.equal(true);
    expect(await usdc.balanceOf(buyer.address)).to.equal(USDC(1000), "the account's wallet paid nothing");
  });

  it("never charges more than the buyer agreed to", async function () {
    const { owner, buyer, checkout } = await deploy();
    await checkout.connect(owner).setPlan(1, USDC(5), 30 * DAY, bit(RAM), true);
    await expect(checkout.connect(buyer).purchase(ACCOUNT, 1, USDC(4), REF))
      .to.be.revertedWithCustomError(checkout, "PriceAboveMax")
      .withArgs(USDC(5), USDC(4));
  });

  it("refuses unknown or retired plans and the zero account", async function () {
    const { owner, buyer, checkout } = await deploy();
    await expect(checkout.connect(buyer).purchase(ACCOUNT, 99, USDC(100), REF))
      .to.be.revertedWithCustomError(checkout, "UnknownPlan")
      .withArgs(99);
    await checkout.connect(owner).setPlan(1, 0, 0, 0, false);
    await expect(checkout.connect(buyer).purchase(ACCOUNT, 1, USDC(4), REF)).to.be.revertedWithCustomError(
      checkout,
      "UnknownPlan",
    );
    await expect(checkout.connect(buyer).purchase(ethers.ZeroHash, 3, USDC(6), REF)).to.be.revertedWithCustomError(
      checkout,
      "ZeroAccount",
    );
  });

  it("fails without allowance or balance, and changes nothing", async function () {
    const { other, checkout } = await deploy();
    await expect(checkout.connect(other).purchase(ACCOUNT, 1, USDC(4), REF)).to.be.reverted;
    expect(await checkout.paidUntil(ACCOUNT, RAM)).to.equal(0n);
  });

  it("pays and approves in one transaction with a permit, and tolerates a front-run permit", async function () {
    const { buyer, treasury, other, usdc, checkout } = await deploy();
    await usdc.connect(buyer).approve(await checkout.getAddress(), 0);
    const deadline = (await now()) + DAY;
    const sign = async (value) => {
      const domain = {
        name: "USD Coin",
        version: "1",
        chainId: (await ethers.provider.getNetwork()).chainId,
        verifyingContract: await usdc.getAddress(),
      };
      const types = {
        Permit: [
          { name: "owner", type: "address" },
          { name: "spender", type: "address" },
          { name: "value", type: "uint256" },
          { name: "nonce", type: "uint256" },
          { name: "deadline", type: "uint256" },
        ],
      };
      const message = {
        owner: buyer.address,
        spender: await checkout.getAddress(),
        value,
        nonce: await usdc.nonces(buyer.address),
        deadline,
      };
      return ethers.Signature.from(await buyer.signTypedData(domain, types, message));
    };

    let sig = await sign(USDC(4));
    await checkout.connect(buyer).purchaseWithPermit(ACCOUNT, 1, USDC(4), REF, deadline, sig.v, sig.r, sig.s);
    expect(await usdc.balanceOf(treasury.address)).to.equal(USDC(4));

    // Someone copies the permit from the mempool and submits it first.
    sig = await sign(USDC(4));
    await usdc.connect(other).permit(buyer.address, await checkout.getAddress(), USDC(4), deadline, sig.v, sig.r, sig.s);
    await checkout.connect(buyer).purchaseWithPermit(ACCOUNT, 1, USDC(4), REF, deadline, sig.v, sig.r, sig.s);
    expect(await usdc.balanceOf(treasury.address)).to.equal(USDC(8));
  });

  it("pausing stops purchases, and only the owner can pause or change plans", async function () {
    const { owner, buyer, other, checkout } = await deploy();
    await checkout.connect(owner).pause();
    await expect(checkout.connect(buyer).purchase(ACCOUNT, 1, USDC(4), REF)).to.be.revertedWithCustomError(
      checkout,
      "EnforcedPause",
    );
    await checkout.connect(owner).unpause();
    await checkout.connect(buyer).purchase(ACCOUNT, 1, USDC(4), REF);

    const token = await checkout.token();
    for (const call of [
      () => checkout.connect(other).pause(),
      () => checkout.connect(other).setPlan(7, USDC(1), DAY, 1, true),
      () => checkout.connect(other).setTreasury(other.address),
      () => checkout.connect(other).rescue(token, other.address, 1),
    ]) {
      await expect(call()).to.be.revertedWithCustomError(checkout, "OwnableUnauthorizedAccount");
    }
  });

  it("refuses plans that are free, empty or too long, and passes too far ahead", async function () {
    const { owner, buyer, checkout } = await deploy();
    for (const [price, duration, wardens] of [
      [0, DAY, 1],
      [USDC(1), 0, 1],
      [USDC(1), DAY, 0],
      [USDC(1), 6 * 365 * DAY, 1],
    ]) {
      await expect(checkout.connect(owner).setPlan(9, price, duration, wardens, true)).to.be.revertedWithCustomError(
        checkout,
        "BadPlan",
      );
    }

    await checkout.connect(owner).setPlan(9, USDC(1), 4 * 365 * DAY, bit(RAM), true);
    await checkout.connect(buyer).purchase(ACCOUNT, 9, USDC(1), REF);
    await expect(checkout.connect(buyer).purchase(ACCOUNT, 9, USDC(1), REF)).to.be.revertedWithCustomError(
      checkout,
      "TooFarAhead",
    );
  });

  it("ownership moves in two steps and cannot be renounced", async function () {
    const { owner, other, checkout } = await deploy();
    await expect(checkout.connect(owner).renounceOwnership()).to.be.revertedWithCustomError(checkout, "RenounceDisabled");
    await checkout.connect(owner).transferOwnership(other.address);
    expect(await checkout.owner()).to.equal(owner.address, "not until accepted");
    await checkout.connect(other).acceptOwnership();
    expect(await checkout.owner()).to.equal(other.address);
  });

  it("returns tokens sent to it by mistake", async function () {
    const { owner, buyer, usdc, checkout } = await deploy();
    await usdc.connect(buyer).transfer(await checkout.getAddress(), USDC(3));
    await checkout.connect(owner).rescue(await usdc.getAddress(), buyer.address, USDC(3));
    expect(await usdc.balanceOf(buyer.address)).to.equal(USDC(1000));
  });

  it("rejects a zero token or treasury at deployment", async function () {
    const [owner] = await ethers.getSigners();
    const F = await ethers.getContractFactory("RamWardenCheckout");
    await expect(F.deploy(ethers.ZeroAddress, owner.address, owner.address)).to.be.revertedWithCustomError(F, "ZeroAddress");
    await expect(F.deploy(owner.address, ethers.ZeroAddress, owner.address)).to.be.revertedWithCustomError(F, "ZeroAddress");
  });
});

describe("plans.json", function () {
  const plans = require("../plans.json");

  it("every plan is valid for setPlan and names known wardens", async function () {
    const [owner] = await ethers.getSigners();
    const usdc = await (await ethers.getContractFactory("MockUSDC")).deploy();
    const checkout = await (await ethers.getContractFactory("RamWardenCheckout")).deploy(
      await usdc.getAddress(),
      owner.address,
      owner.address,
    );
    const ids = new Set();
    for (const p of plans.plans) {
      expect(ids.has(p.id), `duplicate plan id ${p.id}`).to.equal(false);
      ids.add(p.id);
      for (const w of p.wardens) expect(plans.wardens[String(w)], `plan ${p.id} names warden ${w}`).to.exist;
      const mask = p.wardens.reduce((m, w) => m | (1 << w), 0);
      await checkout.setPlan(p.id, BigInt(p.usd) * 1_000_000n, p.days * DAY, mask, true);
    }
  });
});
