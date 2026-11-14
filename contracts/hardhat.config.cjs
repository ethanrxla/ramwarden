// The compiler is the solc-js build pinned in package.json, not one Hardhat
// downloads at run time: the build is the same on every machine and in CI, and
// works where binaries.soliditylang.org is unreachable.
const { subtask } = require("hardhat/config");
const { TASK_COMPILE_SOLIDITY_GET_SOLC_BUILD } = require("hardhat/builtin-tasks/task-names");
require("@nomicfoundation/hardhat-ethers");
require("@nomicfoundation/hardhat-chai-matchers");

const SOLC = "0.8.28";

subtask(TASK_COMPILE_SOLIDITY_GET_SOLC_BUILD, async (args, _hre, runSuper) => {
  if (args.solcVersion !== SOLC) return runSuper();
  return {
    compilerPath: require.resolve("solc/soljson.js"),
    isSolcJs: true,
    version: SOLC,
    longVersion: require("solc").version().replace(".Emscripten.clang", ""),
  };
});

// The deployer key for deploy.cjs, the keeper key for keeper.cjs; never both in one shell.
const key = process.env.DEPLOYER_KEY || process.env.KEEPER_KEY;
const accounts = key ? [key] : [];

module.exports = {
  solidity: { version: SOLC, settings: { optimizer: { enabled: true, runs: 200 }, evmVersion: "cancun" } },
  networks: {
    base: { url: process.env.BASE_RPC_URL || "https://mainnet.base.org", chainId: 8453, accounts },
    baseSepolia: { url: process.env.BASE_SEPOLIA_RPC_URL || "https://sepolia.base.org", chainId: 84532, accounts },
  },
};
