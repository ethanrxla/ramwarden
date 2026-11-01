// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {SafeERC20} from "@openzeppelin/contracts/token/ERC20/utils/SafeERC20.sol";
import {Ownable, Ownable2Step} from "@openzeppelin/contracts/access/Ownable2Step.sol";
import {Pausable} from "@openzeppelin/contracts/utils/Pausable.sol";

/// @title RamWarden checkout: subscriptions with a 14-day trial, paid in USDC.
///
/// @notice A plan is a price per period (a month or a year) and the set of
/// wardens it unlocks (RamWarden: the RAM warden; Pro: the RAM warden and
/// ThreatWarden). A wallet starts a trial by approving the contract to spend
/// USDC and calling `startTrial`; no money moves. When the trial ends, and
/// every period after, anyone may call `charge`, which pulls exactly one
/// period's price from that wallet to the treasury and extends the account's
/// wardens to the end of the new period. `cancel` stops future charges;
/// service runs to the end of what was paid. `paidUntil` is public, so the
/// daemon can check a licence on-chain when the RamWarden API is down.
///
/// What the contract guarantees the subscriber, whatever the operator does:
/// - It never pulls more than the price fixed when the subscription started,
///   never before the period is due, never twice for one period, and never
///   for a plan the subscriber did not pick. A later price change applies to
///   new subscriptions only.
/// - It only ever pulls from the wallet that started the subscription, and
///   only that wallet can cancel it. Revoking the USDC approval in the wallet
///   also stops every future charge.
/// - The approval itself is the subscriber's cap: the website and the agent
///   skill ask for one year of the plan, never an unlimited amount.
/// - Funds never rest here: each charge moves straight to the treasury.
contract RamWardenCheckout is Ownable2Step, Pausable {
    using SafeERC20 for IERC20;

    struct Plan {
        uint128 price; // in the token's smallest unit (USDC: 6 decimals)
        uint32 period; // seconds: 30 days or 365 days
        uint32 wardens; // bit w set = warden w is unlocked
        bool trial; // whether a first subscription starts with the 14-day trial
        bool active;
    }

    struct Subscription {
        address payer;
        uint32 planId;
        uint128 price; // fixed at start
        uint32 period; // fixed at start
        uint32 wardens; // fixed at start
        uint64 nextCharge; // when the next period is due
        bool active;
    }

    uint8 public constant MAX_WARDENS = 32;
    uint64 public constant TRIAL = 14 days;
    /// A charge this late starts the new period now instead of back-dating it
    /// to when it was due, so a lapsed subscriber is never billed for time
    /// they had no service.
    uint64 public constant GRACE = 3 days;
    uint32 public constant MAX_PERIOD = 366 days;

    IERC20 public immutable token;
    address public treasury;

    mapping(uint32 planId => Plan) public plans;
    mapping(bytes32 account => Subscription) public subscriptions;
    /// One trial per account, ever.
    mapping(bytes32 account => bool) public trialUsed;
    mapping(bytes32 account => mapping(uint8 warden => uint64 until)) public paidUntil;

    /// `trialEnds` is 0 when the plan has no trial or the account used its
    /// trial; a `Charged` for the first period follows in the same call.
    event TrialStarted(bytes32 indexed account, uint32 indexed planId, address indexed payer, uint64 trialEnds, bytes32 ref);
    event Charged(bytes32 indexed account, uint32 indexed planId, address indexed payer, uint256 price, uint64 paidUntil);
    event Cancelled(bytes32 indexed account, uint64 serviceUntil);
    event Extended(bytes32 indexed account, uint8 indexed warden, uint64 until);
    event PlanSet(uint32 indexed planId, uint128 price, uint32 period, uint32 wardens, bool trial, bool active);
    event TreasurySet(address treasury);

    error ZeroAddress();
    error ZeroAccount();
    error UnknownPlan(uint32 planId);
    error PriceAboveMax(uint256 price, uint256 maxPrice);
    error AllowanceTooLow(uint256 allowance, uint256 price);
    error AlreadySubscribed();
    error NotSubscribed();
    error NotDue(uint64 nextCharge);
    error NotPayer();
    error BadPlan();
    error RenounceDisabled();

    constructor(IERC20 token_, address treasury_, address owner_) Ownable(owner_) {
        if (address(token_) == address(0) || treasury_ == address(0)) revert ZeroAddress();
        token = token_;
        treasury = treasury_;
        emit TreasurySet(treasury_);
    }

    // ---------------------------------------------------------- subscriber

    /// Subscribe `account` to `planId`, paid from the caller's wallet. The
    /// first 14 days are free if the plan offers a trial and this account never
    /// had one; otherwise the first period is charged now. The caller must already have approved at
    /// least one period's price, so the trial can convert without another step.
    function startTrial(bytes32 account, uint32 planId, uint256 maxPrice, bytes32 ref) external whenNotPaused {
        if (account == bytes32(0)) revert ZeroAccount();
        Plan memory p = plans[planId];
        if (!p.active) revert UnknownPlan(planId);
        if (p.price > maxPrice) revert PriceAboveMax(p.price, maxPrice);
        if (subscriptions[account].active) revert AlreadySubscribed();
        uint256 allowance = token.allowance(msg.sender, address(this));
        if (allowance < p.price) revert AllowanceTooLow(allowance, p.price);

        uint64 nowTs = uint64(block.timestamp);
        subscriptions[account] = Subscription({
            payer: msg.sender,
            planId: planId,
            price: p.price,
            period: p.period,
            wardens: p.wardens,
            nextCharge: nowTs,
            active: true
        });

        if (p.trial && !trialUsed[account]) {
            trialUsed[account] = true;
            uint64 trialEnds = nowTs + TRIAL;
            subscriptions[account].nextCharge = trialEnds;
            _extend(account, p.wardens, trialEnds);
            emit TrialStarted(account, planId, msg.sender, trialEnds, ref);
        } else {
            emit TrialStarted(account, planId, msg.sender, 0, ref);
            _charge(account);
        }
    }

    /// Stop future charges. Service runs to the end of the paid period.
    function cancel(bytes32 account) external {
        Subscription storage s = subscriptions[account];
        if (!s.active) revert NotSubscribed();
        if (msg.sender != s.payer) revert NotPayer();
        s.active = false;
        emit Cancelled(account, s.nextCharge);
    }

    // ---------------------------------------------------------- keeper

    /// Charge one due period. Anyone may call it (RamWarden runs a keeper);
    /// it reverts unless a period is due, and the amount and payer are fixed.
    function charge(bytes32 account) external whenNotPaused {
        _charge(account);
    }

    function _charge(bytes32 account) private {
        Subscription storage s = subscriptions[account];
        if (!s.active) revert NotSubscribed();
        uint64 nowTs = uint64(block.timestamp);
        if (nowTs < s.nextCharge) revert NotDue(s.nextCharge);

        uint64 start = nowTs > s.nextCharge + GRACE ? nowTs : s.nextCharge;
        uint64 until = start + s.period;
        s.nextCharge = until;
        _extend(account, s.wardens, until);

        token.safeTransferFrom(s.payer, treasury, s.price);
        emit Charged(account, s.planId, s.payer, s.price, until);
    }

    function _extend(bytes32 account, uint32 wardens, uint64 until) private {
        for (uint8 w = 0; w < MAX_WARDENS; ++w) {
            if (wardens & (uint32(1) << w) == 0) continue;
            if (paidUntil[account][w] < until) {
                paidUntil[account][w] = until;
                emit Extended(account, w, until);
            }
        }
    }

    // ---------------------------------------------------------- reading

    function isActive(bytes32 account, uint8 warden) external view returns (bool) {
        return paidUntil[account][warden] > block.timestamp;
    }

    /// Whether `charge(account)` would be accepted now, apart from the
    /// payer's balance and allowance.
    function isDue(bytes32 account) external view returns (bool) {
        Subscription memory s = subscriptions[account];
        return s.active && block.timestamp >= s.nextCharge;
    }

    // ---------------------------------------------------------- owner

    function setPlan(uint32 planId, uint128 price, uint32 period, uint32 wardens, bool trial, bool active)
        external
        onlyOwner
    {
        if (active && (price == 0 || period < 1 days || period > MAX_PERIOD || wardens == 0)) revert BadPlan();
        plans[planId] = Plan(price, period, wardens, trial, active);
        emit PlanSet(planId, price, period, wardens, trial, active);
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

    /// Return tokens someone sent here directly.
    function rescue(IERC20 token_, address to, uint256 amount) external onlyOwner {
        if (to == address(0)) revert ZeroAddress();
        token_.safeTransfer(to, amount);
    }

    /// Without an owner no plan could ever be changed or paused again.
    function renounceOwnership() public view override onlyOwner {
        revert RenounceDisabled();
    }
}
