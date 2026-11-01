// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {IERC20Permit} from "@openzeppelin/contracts/token/ERC20/extensions/IERC20Permit.sol";
import {SafeERC20} from "@openzeppelin/contracts/token/ERC20/utils/SafeERC20.sol";
import {Ownable, Ownable2Step} from "@openzeppelin/contracts/access/Ownable2Step.sol";
import {Pausable} from "@openzeppelin/contracts/utils/Pausable.sol";

/// @title RamWarden checkout: prepaid warden passes, paid in USDC.
///
/// @notice A plan is a price, a duration and a set of wardens (the RAM warden's
/// cloud tier, the Wazuh warden add-on, ...). Buying one extends each of those
/// wardens for the account by the plan's duration, from today or from the
/// current expiry, whichever is later. `paidUntil` is public, so anyone can
/// check a pass on-chain; the RamWarden server reads it back as the source of
/// truth after each `Purchased` event.
///
/// Design choices, each to keep what can go wrong small:
/// - Funds never rest here. Every payment moves from the payer straight to the
///   treasury (a Safe) in the same call, so a bug can miscount days but cannot
///   lose money held by the contract.
/// - USDC only, priced in its own units. No ETH, no oracle, no swap.
/// - No upgrade proxy. A new version is a new deployment.
/// - The buyer names the most they will pay (`maxPrice`), so a price change
///   between quote and inclusion cannot charge them more than they agreed to.
/// - `ref` ties the payment to the server's payment record; it is only logged.
contract RamWardenCheckout is Ownable2Step, Pausable {
    using SafeERC20 for IERC20;

    struct Plan {
        uint128 price; // in the token's smallest unit (USDC: 6 decimals)
        uint32 duration; // seconds
        uint32 wardens; // bit w set = warden w is included
        bool active;
    }

    /// Warden ids are bit positions in `Plan.wardens`.
    uint8 public constant MAX_WARDENS = 32;
    /// Longest a pass may run ahead of now, so a mistyped duration cannot sell
    /// a century.
    uint64 public constant MAX_AHEAD = 5 * 365 days;

    IERC20 public immutable token;
    address public treasury;

    mapping(uint32 planId => Plan) public plans;
    mapping(bytes32 account => mapping(uint8 warden => uint64 until)) public paidUntil;

    event Purchased(
        bytes32 indexed account,
        uint32 indexed planId,
        address indexed payer,
        uint256 price,
        uint32 wardens,
        bytes32 ref
    );
    event Extended(bytes32 indexed account, uint8 indexed warden, uint64 until);
    event PlanSet(uint32 indexed planId, uint128 price, uint32 duration, uint32 wardens, bool active);
    event TreasurySet(address treasury);

    error ZeroAddress();
    error ZeroAccount();
    error UnknownPlan(uint32 planId);
    error PriceAboveMax(uint256 price, uint256 maxPrice);
    error BadPlan();
    error TooFarAhead(uint8 warden, uint64 until);
    error RenounceDisabled();

    constructor(IERC20 token_, address treasury_, address owner_) Ownable(owner_) {
        if (address(token_) == address(0) || treasury_ == address(0)) revert ZeroAddress();
        token = token_;
        treasury = treasury_;
        emit TreasurySet(treasury_);
    }

    // ---------------------------------------------------------------- buying

    /// Pay for `planId` on behalf of `account`. The caller must have approved at
    /// least the plan's price to this contract.
    function purchase(bytes32 account, uint32 planId, uint256 maxPrice, bytes32 ref) external whenNotPaused {
        _purchase(account, planId, maxPrice, ref);
    }

    /// Same as `purchase`, approving with an EIP-2612 permit in the same
    /// transaction. A permit that was already used (for example front-run by
    /// someone who saw it in the mempool) is tolerated: if the allowance is
    /// there, the payment goes ahead; if not, the transfer reverts.
    function purchaseWithPermit(
        bytes32 account,
        uint32 planId,
        uint256 maxPrice,
        bytes32 ref,
        uint256 deadline,
        uint8 v,
        bytes32 r,
        bytes32 s
    ) external whenNotPaused {
        try IERC20Permit(address(token)).permit(msg.sender, address(this), maxPrice, deadline, v, r, s) {} catch {}
        _purchase(account, planId, maxPrice, ref);
    }

    function _purchase(bytes32 account, uint32 planId, uint256 maxPrice, bytes32 ref) private {
        if (account == bytes32(0)) revert ZeroAccount();
        Plan memory p = plans[planId];
        if (!p.active) revert UnknownPlan(planId);
        if (p.price > maxPrice) revert PriceAboveMax(p.price, maxPrice);

        // Effects before the transfer; the token is trusted USDC, but the order
        // costs nothing.
        uint64 nowTs = uint64(block.timestamp);
        for (uint8 w = 0; w < MAX_WARDENS; ++w) {
            if (p.wardens & (uint32(1) << w) == 0) continue;
            uint64 current = paidUntil[account][w];
            uint64 until = (current > nowTs ? current : nowTs) + p.duration;
            if (until > nowTs + MAX_AHEAD) revert TooFarAhead(w, until);
            paidUntil[account][w] = until;
            emit Extended(account, w, until);
        }

        token.safeTransferFrom(msg.sender, treasury, p.price);
        emit Purchased(account, planId, msg.sender, p.price, p.wardens, ref);
    }

    // ---------------------------------------------------------------- reading

    function isActive(bytes32 account, uint8 warden) external view returns (bool) {
        return paidUntil[account][warden] > block.timestamp;
    }

    // ---------------------------------------------------------------- owner

    function setPlan(uint32 planId, uint128 price, uint32 duration, uint32 wardens, bool active) external onlyOwner {
        if (active && (price == 0 || duration == 0 || wardens == 0 || duration > MAX_AHEAD)) revert BadPlan();
        plans[planId] = Plan(price, duration, wardens, active);
        emit PlanSet(planId, price, duration, wardens, active);
    }

    function setTreasury(address treasury_) external onlyOwner {
        if (treasury_ == address(0)) revert ZeroAddress();
        treasury = treasury_;
        emit TreasurySet(treasury_);
    }

    function pause() external onlyOwner {
        _pause();
    }

    function unpause() external onlyOwner {
        _unpause();
    }

    /// Return tokens someone sent here directly instead of through `purchase`.
    function rescue(IERC20 token_, address to, uint256 amount) external onlyOwner {
        if (to == address(0)) revert ZeroAddress();
        token_.safeTransfer(to, amount);
    }

    /// Without an owner no plan could ever be changed or paused again.
    function renounceOwnership() public view override onlyOwner {
        revert RenounceDisabled();
    }
}
