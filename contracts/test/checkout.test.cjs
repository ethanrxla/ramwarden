const { expect } = require("chai");
const { ethers, network } = require("hardhat");

const DAY = 24 * 60 * 60;
const MONTH = 30 * DAY;
const YEAR = 365 * DAY;
const USDC = (n) => BigInt(Math.round(n * 1e6));
const RAM = 0; // the RAM warden
const THREAT = 1; // ThreatWarden
const bit = (w) => 1 << w;
const ACCOUNT = ethers.id("account:65f0c0ffee");
const REF = ethers.id("payment:1");
const [RAM_MONTH, PRO_MONTH, RAM_YEAR] = [1, 2, 3];

async function now() {
  return (await ethers.provider.getBlock("latest")).timestamp;
}

async function forward(seconds) {
  await network.provider.send("evm_increaseTime", [seconds]);
  await network.provider.send("evm_mine");
}

async function deploy() {
  const [owner, treasury, buyer, other, keeper] = await ethers.getSigners();
  const usdc = await (await ethers.getContractFactory("MockUSDC")).deploy();
  const checkout = await (await ethers.getContractFactory("RamWardenCheckout")).deploy(
    await usdc.getAddress(),
    treasury.address,
    owner.address,
  );
  await checkout.setPlan(RAM_MONTH, USDC(4), MONTH, bit(RAM), true);
  await checkout.setPlan(PRO_MONTH, USDC(5), MONTH, bit(RAM) | bit(THREAT), true);
  await checkout.setPlan(RAM_YEAR, USDC(20), YEAR, bit(RAM), true);
  await usdc.mint(buyer.address, USDC(1000));
  // What the website asks for: one year of the monthly plan, never unlimited.
  await usdc.connect(buyer).approve(await checkout.getAddress(), USDC(12 * 5));
  return { owner, treasury, buyer, other, keeper, usdc, checkout };
}

describe("RamWardenCheckout", function () {
  it("a trial moves no money and unlocks the plan's wardens for 14 days", async function () {
    const { treasury, buyer, usdc, checkout } = await deploy();
    const tx = await checkout.connect(buyer).startTrial(ACCOUNT, PRO_MONTH, USDC(5), REF);
    const t = await now();
    await expect(tx).to.emit(checkout, "TrialStarted").withArgs(ACCOUNT, PRO_MONTH, buyer.address, t + 14 * DAY, REF);
    expect(await usdc.balanceOf(treasury.address)).to.equal(0n);
    expect(await checkout.paidUntil(ACCOUNT, RAM)).to.equal(t + 14 * DAY);
    expect(await checkout.paidUntil(ACCOUNT, THREAT)).to.equal(t + 14 * DAY);
    expect(await checkout.isDue(ACCOUNT)).to.equal(false);
  });

  it("charges exactly one period when due, never early, never twice", async function () {
    const { treasury, buyer, keeper, usdc, checkout } = await deploy();
    await checkout.connect(buyer).startTrial(ACCOUNT, RAM_MONTH, USDC(4), REF);
    const trialEnds = (await checkout.subscriptions(ACCOUNT)).nextCharge;

    await expect(checkout.connect(keeper).charge(ACCOUNT)).to.be.revertedWithCustomError(checkout, "NotDue");
    await forward(14 * DAY);
    await expect(checkout.connect(keeper).charge(ACCOUNT))
      .to.emit(checkout, "Charged")
      .withArgs(ACCOUNT, RAM_MONTH, buyer.address, USDC(4), trialEnds + BigInt(MONTH));
    expect(await usdc.balanceOf(treasury.address)).to.equal(USDC(4));
    expect(await usdc.balanceOf(await checkout.getAddress())).to.equal(0n, "nothing rests in the contract");
    expect(await checkout.paidUntil(ACCOUNT, RAM)).to.equal(trialEnds + BigInt(MONTH));
    expect(await checkout.paidUntil(ACCOUNT, THREAT)).to.equal(0n, "RamWarden does not unlock ThreatWarden");
    await expect(checkout.connect(keeper).charge(ACCOUNT)).to.be.revertedWithCustomError(checkout, "NotDue");

    await forward(MONTH);
    await checkout.connect(keeper).charge(ACCOUNT);
    expect(await usdc.balanceOf(treasury.address)).to.equal(USDC(8));
    expect(await checkout.paidUntil(ACCOUNT, RAM)).to.equal(trialEnds + BigInt(2 * MONTH), "periods stay on cadence");
  });

  it("a price change never reaches existing subscribers", async function () {
    const { owner, treasury, buyer, usdc, checkout } = await deploy();
    await checkout.connect(buyer).startTrial(ACCOUNT, RAM_MONTH, USDC(4), REF);
    await checkout.connect(owner).setPlan(RAM_MONTH, USDC(9), MONTH, bit(RAM) | bit(THREAT), true);
    await forward(14 * DAY);
    await checkout.charge(ACCOUNT);
    expect(await usdc.balanceOf(treasury.address)).to.equal(USDC(4));
    expect(await checkout.paidUntil(ACCOUNT, THREAT)).to.equal(0n, "nor does a widened plan");
  });

  it("a charge far past due starts the period now, so no one pays for time they lacked", async function () {
    const { buyer, checkout } = await deploy();
    await checkout.connect(buyer).startTrial(ACCOUNT, RAM_MONTH, USDC(4), REF);
    await forward(14 * DAY + 10 * DAY);
    await checkout.charge(ACCOUNT);
    expect(await checkout.paidUntil(ACCOUNT, RAM)).to.equal(BigInt((await now()) + MONTH));
  });

  it("a short balance or revoked approval fails the charge and changes nothing", async function () {
    const { buyer, usdc, checkout } = await deploy();
    await checkout.connect(buyer).startTrial(ACCOUNT, RAM_MONTH, USDC(4), REF);
    const before = await checkout.paidUntil(ACCOUNT, RAM);
    await usdc.connect(buyer).approve(await checkout.getAddress(), 0);
    await forward(14 * DAY);
    await expect(checkout.charge(ACCOUNT)).to.be.reverted;
    expect(await checkout.paidUntil(ACCOUNT, RAM)).to.equal(before);
    expect(await checkout.isDue(ACCOUNT)).to.equal(true, "still due, so the keeper retries");
  });

  it("only the paying wallet can cancel; service runs to the end of the period", async function () {
    const { buyer, other, checkout } = await deploy();
    await checkout.connect(buyer).startTrial(ACCOUNT, RAM_MONTH, USDC(4), REF);
    await expect(checkout.connect(other).cancel(ACCOUNT)).to.be.revertedWithCustomError(checkout, "NotPayer");
    const until = await checkout.paidUntil(ACCOUNT, RAM);
    await expect(checkout.connect(buyer).cancel(ACCOUNT)).to.emit(checkout, "Cancelled").withArgs(ACCOUNT, until);
    expect(await checkout.isActive(ACCOUNT, RAM)).to.equal(true);
    await forward(14 * DAY);
    await expect(checkout.charge(ACCOUNT)).to.be.revertedWithCustomError(checkout, "NotSubscribed");
    expect(await checkout.isActive(ACCOUNT, RAM)).to.equal(false);
  });

  it("one trial per account: subscribing again charges the first period at once", async function () {
    const { treasury, buyer, usdc, checkout } = await deploy();
    await checkout.connect(buyer).startTrial(ACCOUNT, RAM_MONTH, USDC(4), REF);
    await checkout.connect(buyer).cancel(ACCOUNT);
    await checkout.connect(buyer).startTrial(ACCOUNT, PRO_MONTH, USDC(5), REF);
    expect(await usdc.balanceOf(treasury.address)).to.equal(USDC(5));
    expect(await checkout.paidUntil(ACCOUNT, THREAT)).to.equal(BigInt((await now()) + MONTH));
  });

  it("refuses a second subscription, a missing approval, a higher price and unknown plans", async function () {
    const { owner, buyer, other, usdc, checkout } = await deploy();
    await expect(checkout.connect(other).startTrial(ACCOUNT, RAM_MONTH, USDC(4), REF))
      .to.be.revertedWithCustomError(checkout, "AllowanceTooLow")
      .withArgs(0, USDC(4));
    await expect(checkout.connect(buyer).startTrial(ACCOUNT, RAM_MONTH, USDC(3), REF))
      .to.be.revertedWithCustomError(checkout, "PriceAboveMax")
      .withArgs(USDC(4), USDC(3));
    await expect(checkout.connect(buyer).startTrial(ACCOUNT, 99, USDC(100), REF))
      .to.be.revertedWithCustomError(checkout, "UnknownPlan")
      .withArgs(99);
    await expect(checkout.connect(buyer).startTrial(ethers.ZeroHash, RAM_MONTH, USDC(4), REF)).to.be.revertedWithCustomError(
      checkout,
      "ZeroAccount",
    );
    await checkout.connect(buyer).startTrial(ACCOUNT, RAM_MONTH, USDC(4), REF);
    await usdc.mint(other.address, USDC(10));
    await usdc.connect(other).approve(await checkout.getAddress(), USDC(10));
    await expect(checkout.connect(other).startTrial(ACCOUNT, RAM_MONTH, USDC(4), REF)).to.be.revertedWithCustomError(
      checkout,
      "AlreadySubscribed",
    );
    await checkout.connect(owner).setPlan(RAM_YEAR, 0, 0, 0, false);
    await expect(checkout.connect(other).startTrial(ethers.id("x"), RAM_YEAR, USDC(20), REF)).to.be.revertedWithCustomError(
      checkout,
      "UnknownPlan",
    );
  });

  it("pausing stops new trials and charges but never stops a cancel", async function () {
    const { owner, buyer, other, checkout } = await deploy();
    await checkout.connect(buyer).startTrial(ACCOUNT, RAM_MONTH, USDC(4), REF);
    await checkout.connect(owner).pause();
    await expect(checkout.connect(buyer).startTrial(ethers.id("y"), RAM_MONTH, USDC(4), REF)).to.be.revertedWithCustomError(
      checkout,
      "EnforcedPause",
    );
    await forward(14 * DAY);
    await expect(checkout.charge(ACCOUNT)).to.be.revertedWithCustomError(checkout, "EnforcedPause");
    await checkout.connect(buyer).cancel(ACCOUNT);

    const token = await checkout.token();
    for (const call of [
      () => checkout.connect(other).unpause(),
      () => checkout.connect(other).setPlan(7, USDC(1), DAY, 1, true),
      () => checkout.connect(other).setTreasury(other.address),
      () => checkout.connect(other).rescue(token, other.address, 1),
    ]) {
      await expect(call()).to.be.revertedWithCustomError(checkout, "OwnableUnauthorizedAccount");
    }
  });

  it("refuses plans that are free, empty or longer than a year", async function () {
    const { owner, checkout } = await deploy();
    for (const [price, period, wardens] of [
      [0, MONTH, 1],
      [USDC(1), 0, 1],
      [USDC(1), MONTH, 0],
      [USDC(1), 400 * DAY, 1],
    ]) {
      await expect(checkout.connect(owner).setPlan(9, price, period, wardens, true)).to.be.revertedWithCustomError(
        checkout,
        "BadPlan",
      );
    }
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

describe("keeper", function () {
  it("finds only the subscriptions that are due", async function () {
    const { buyer, checkout } = await deploy();
    const { dueAccounts } = require("../scripts/keeper.cjs");
    await checkout.connect(buyer).startTrial(ACCOUNT, RAM_MONTH, USDC(4), REF);
    await checkout.connect(buyer).startTrial(ethers.id("b"), RAM_MONTH, USDC(4), REF);
    expect(await dueAccounts(checkout, 0)).to.deep.equal([]);
    await forward(14 * DAY);
    await checkout.connect(buyer).cancel(ethers.id("b"));
    expect(await dueAccounts(checkout, 0)).to.deep.equal([ACCOUNT]);
  });
});

describe("plans.json", function () {
  const plans = require("../plans.json");

  it("matches the approved spec and every plan is valid for setPlan", async function () {
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
    // docs/product-spec.md 1.2: ids are fixed because the agent skill uses them.
    const by = Object.fromEntries(plans.plans.map((p) => [p.id, [p.tier, p.interval, p.usd]]));
    expect(by).to.deep.equal({ 1: ["ram", "month", 4], 2: ["pro", "month", 5], 3: ["ram", "year", 20], 4: ["pro", "year", 24] });
  });
});
