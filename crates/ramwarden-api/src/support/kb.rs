//! What the site wardens know: one curated answer per topic.
//!
//! # Why the answers are written, not generated
//!
//! The pets sell. A sales answer that invents a price, a device count or a
//! refund promise is worse than no answer, and the decision model behind the
//! wardens answers typed questions rather than writing prose. So the model
//! *routes* (which topic is this, does the visitor need a person) and every
//! word the visitor reads comes from this file, checked against
//! `docs/product-spec.md` and `web/assets/config.js`. When a price or a plan
//! changes, it changes here in the same commit, and the test at the bottom
//! pins the numbers so a stale answer fails CI.
//!
//! Site copy rules apply: no em dashes, no engine or model names, and "if it
//! doesn't know, it says so and offers a ticket" (spec 2.6).

/// One thing a visitor might ask about.
pub struct Topic {
    pub id: &'static str,
    /// What the topic covers, as the decision model reads it.
    pub meaning: &'static str,
    /// Lowercase words and phrases for the fallback router. A phrase scores
    /// more than a word, so "free trial" beats "free".
    pub keywords: &'static [&'static str],
    pub answer: &'static str,
    /// Follow-up chips shown under the answer: topic ids.
    pub next: &'static [&'static str],
    /// Offer to open a ticket under this answer.
    pub ticket: bool,
}

/// The question chips a fresh chat shows, per warden.
pub fn starters(warden: &str) -> &'static [&'static str] {
    match warden {
        "threat" => &["threatwarden", "pricing", "privacy", "platforms"],
        _ => &["plans", "trial", "install", "how_it_works"],
    }
}

pub fn greeting(warden: &str) -> &'static str {
    match warden {
        "threat" => {
            "I'm ThreatWarden's warden. Ask me about security, plans or setup. If I can't answer, I can help you open a ticket for the team."
        }
        _ => "I'm your warden. Ask me about plans, the free trial, installing, or how I free memory. If I can't answer, I can help you open a ticket for the team.",
    }
}

/// The chip label for a topic: what a visitor would type.
pub fn chip(id: &str) -> &'static str {
    match id {
        "plans" => "Which plan is right for me?",
        "pricing" => "How much does it cost?",
        "trial" => "How does the free trial work?",
        "lite" => "What's in the free plan?",
        "devices" => "How many computers can I use?",
        "install" => "How do I install it?",
        "platforms" => "Does it work on my computer?",
        "extension" => "What does the browser extension do?",
        "payment" => "How can I pay?",
        "signin" => "How do I sign in?",
        "cancel" => "How do I cancel?",
        "refund" => "Can I get a refund?",
        "privacy" => "What data leaves my computer?",
        "safety" => "Will it close something I need?",
        "how_it_works" => "How does it free memory?",
        "threatwarden" => "What does ThreatWarden do?",
        "open_source" => "Is it open source?",
        "my_account" => "What plan am I on?",
        "human" => "Talk to a person",
        _ => "Ask something else",
    }
}

pub const UNKNOWN: &str = "unknown";

pub fn unknown_answer() -> &'static str {
    "I don't know that one for sure, and I'd rather not guess. I can open a ticket so a person on the team answers you."
}

pub const TOPICS: &[Topic] = &[
    Topic {
        id: "plans",
        meaning: "comparing the plans or tiers (Lite, Plus, Pro), or which one to choose",
        keywords: &["plan", "plans", "tier", "tiers", "which one", "difference", "compare", "plus", "pro", "lite", "upgrade"],
        answer: "There are three plans. RamWarden Lite is free: the memory algorithm with no AI, on 1 computer. \
                 RamWarden Plus adds the AI warden pet that helps choose idle apps and tabs, on 3 computers, for $4 every 2 months or $20 a year, with a 14-day free trial. \
                 RamWarden Pro adds ThreatWarden, the security companion, on 5 computers, for $5 every 2 months or $24 a year. \
                 If you mostly want memory back, start with the Plus trial. If you also want security, Pro.",
        next: &["trial", "lite", "threatwarden"],
        ticket: false,
    },
    Topic {
        id: "pricing",
        meaning: "how much it costs, prices, billing period, monthly or yearly",
        keywords: &["price", "prices", "cost", "costs", "how much", "$", "dollar", "cheap", "expensive", "per month", "monthly", "yearly", "annual", "billing"],
        answer: "Lite is free. Plus is $4 every 2 months or $20 a year. Pro is $5 every 2 months or $24 a year. \
                 You're charged every 60 days or once a year, whichever you pick, until you cancel. The yearly plans save about 17% for Plus and 20% for Pro.",
        next: &["trial", "payment", "cancel"],
        ticket: false,
    },
    Topic {
        id: "trial",
        meaning: "the free trial: how long, which plan has it, what happens when it ends",
        keywords: &["trial", "free trial", "try", "try it", "14 day", "14-day", "trial end", "after the trial"],
        answer: "RamWarden Plus has a 14-day free trial. Nothing is charged during it. When it ends, the payment method you chose is charged every 2 months or yearly until you cancel, \
                 and if you cancel first you're never charged. There's one trial per account and per computer. Pro is paid from day one, so if you want to try the AI first, start with Plus and upgrade in a few clicks.",
        next: &["cancel", "payment", "plans"],
        ticket: false,
    },
    Topic {
        id: "lite",
        meaning: "the free plan, RamWarden Lite, what you get without paying",
        keywords: &["free", "free plan", "lite", "without paying", "no card", "free version", "free tier"],
        answer: "RamWarden Lite is free with an account (email or wallet, no card). It measures memory the way your OS charges it, acts on memory stalls rather than a full meter, \
                 runs a 4-step reclaim algorithm, keeps a tab budget with a reopen list, and never touches games, anti-cheat, calls, terminals or VMs. \
                 It covers 1 computer. It has no AI and no warden pet; those come with Plus.",
        next: &["plans", "trial", "install"],
        ticket: false,
    },
    Topic {
        id: "devices",
        meaning: "how many computers or devices an account covers, adding or removing a computer",
        keywords: &["device", "devices", "computer", "computers", "machines", "laptop", "desktop", "how many", "second computer", "another computer", "remove a device"],
        answer: "Lite covers 1 computer, Plus 3 and Pro 5, in any mix of Windows, macOS and Linux. \
                 To add one, sign in on the website from that computer and run the install line it gives you. To free a slot, remove a computer on your account page. More than 5 isn't sold yet.",
        next: &["install", "plans", "my_account"],
        ticket: false,
    },
    Topic {
        id: "install",
        meaning: "how to download, install or set up the app",
        keywords: &["install", "installer", "download", "set up", "setup", "get started", "one line", "command", "curl", "powershell", "update"],
        answer: "Sign in on the website (a free Lite account works), then the install section shows a personal one-line command for macOS, Windows or Linux, plus an installer download. \
                 When a release is available, the command works once, for 30 minutes, and signs that computer in for you. If installation is unavailable, I can help you open a ticket.",
        next: &["platforms", "extension", "devices"],
        ticket: true,
    },
    Topic {
        id: "platforms",
        meaning: "which operating systems or hardware it runs on: Windows, macOS, Linux, Apple silicon, Intel",
        keywords: &["windows", "mac", "macos", "linux", "ubuntu", "fedora", "apple silicon", "m1", "m2", "m3", "m4", "intel", "os", "operating system", "support my"],
        answer: "RamWarden runs on Windows 11, macOS on Apple silicon, and Linux. ThreatWarden runs on all three too. \
                 If your setup is unusual, ask me and I'll pass it to the team.",
        next: &["install", "devices"],
        ticket: true,
    },
    Topic {
        id: "extension",
        meaning: "the browser extension, Chrome, tabs, how tabs are handled",
        keywords: &["extension", "chrome", "browser", "edge", "brave", "firefox", "tab", "tabs", "add-on", "plugin"],
        answer: "The browser extension works with the app on your computer: it finds it by itself, with no code to type, and lets your warden identify idle tabs that may be unloaded or closed. \
                 Closed tabs go to a reopen list. Until the store listing is live, the install page shows how to add it to Chrome.",
        next: &["safety", "privacy", "install"],
        ticket: false,
    },
    Topic {
        id: "payment",
        meaning: "how to pay: crypto wallet, MetaMask, Phantom, card, Apple Pay",
        keywords: &["pay", "payment", "wallet", "crypto", "metamask", "phantom", "usdc", "card", "credit card", "apple pay", "google pay", "stripe", "base", "solana"],
        answer: "You can pay with a supported wallet on Base, or with a card when checkout is enabled. Apple Pay availability depends on your device. Solana payments are not available yet. \
                 With a wallet, your wallet shows the exact amount it approves before you sign, and it can never be charged more than one period's price at a time.",
        next: &["trial", "cancel", "signin"],
        ticket: true,
    },
    Topic {
        id: "signin",
        meaning: "signing in, creating an account, email code, wallet sign-in, login problems",
        keywords: &["sign in", "signin", "log in", "login", "account", "sign up", "signup", "register", "email code", "code", "password"],
        answer: "There are two ways in, and either one is your account: an email address (we send a 6-digit code, no password) or a wallet signature. \
                 Use the Account link at the bottom of the page. If a code doesn't arrive or a sign-in fails, I can open a ticket.",
        next: &["install", "payment", "human"],
        ticket: true,
    },
    Topic {
        id: "cancel",
        meaning: "cancelling a subscription, stopping renewal",
        keywords: &["cancel", "cancellation", "stop paying", "unsubscribe", "end my subscription", "stop renewal", "turn off renewal"],
        answer: "You can cancel any time from your account page, or from your wallet if you paid with one. Nothing more is charged, and your plan keeps working until the end of the time you've paid for. \
                 After that, the app keeps running on the free algorithm instead of stopping.",
        next: &["refund", "my_account"],
        ticket: true,
    },
    Topic {
        id: "refund",
        meaning: "refunds, getting money back, being charged by mistake, a double charge",
        keywords: &["refund", "money back", "charged twice", "double charge", "overcharged", "wrong charge", "charged by mistake", "chargeback"],
        answer: "I can't promise a refund myself. Open a ticket with what happened, and a person on the team will look at your payment and write back.",
        next: &["cancel", "human"],
        ticket: true,
    },
    Topic {
        id: "privacy",
        meaning: "privacy, what data is sent or collected, tracking, what leaves the computer",
        keywords: &["privacy", "private", "data", "collect", "tracking", "track", "send", "telemetry", "spy", "gdpr", "delete my data", "store"],
        answer: "Your memory is measured on your computer, and your process list, tabs and settings stay there. When your warden needs a decision on Plus, it sends a short description of the one app or tab in question \
                 (such as its name, how long it has been idle and how much memory it uses), gets an answer, and the description isn't stored. \
                 These chats are kept for 30 days so the team can help, or a year if they become a ticket. The privacy page has the details.",
        next: &["safety", "open_source"],
        ticket: false,
    },
    Topic {
        id: "safety",
        meaning: "whether it might close or kill something important, games, unsaved work, protected apps",
        keywords: &["safe", "safety", "lose", "unsaved", "close my", "kill", "game", "games", "anti-cheat", "important", "protected", "undo", "work"],
        answer: "RamWarden uses activity signals and safety checks before reclaiming memory. Unloading a tab leaves it in the tab strip so it can reload; closing removes it and records it in the reopen list. Automatic housekeeping can run even when memory is not full. App actions depend on your settings and platform. Predictions can be wrong: if a tab closes unexpectedly, reopen it and report it here. Recent-work protection and a Mac review checklist are being improved; they are not yet available in every installed version.",
        next: &["how_it_works", "extension"],
        ticket: false,
    },
    Topic {
        id: "how_it_works",
        meaning: "how it frees memory, how it works, RAM, performance, speed",
        keywords: &["how does", "how it works", "ram", "memory", "slow", "speed", "faster", "performance", "free up", "reclaim", "task manager", "32gb", "16gb"],
        answer: "RamWarden measures memory the way your OS charges it, so shared browser memory is counted once, and it acts on memory stalls (time lost waiting on RAM) rather than a full meter. \
                 Then it works up a ladder in order of how little each step costs you: compress cold memory, page it out, unload idle tabs, and only then pause idle apps you've listed. \
                 Plus adds an AI that predicts what you'll come back to.",
        next: &["safety", "plans"],
        ticket: false,
    },
    Topic {
        id: "threatwarden",
        meaning: "ThreatWarden, security, antivirus, malware, threats, network monitoring",
        keywords: &["threatwarden", "threat", "security", "secure", "antivirus", "virus", "malware", "hack", "hacked", "network", "firewall", "rootkit", "sentinel", "quarantine"],
        answer: "ThreatWarden is the security companion in RamWarden Pro. It monitors file changes and security events, and helps you understand findings. \
                 Antivirus scans and network monitoring are being integrated into the app. Quarantine and blocking controls are planned, not available yet. \
                 Pro covers 5 computers; security setup availability depends on your platform and release.",
        next: &["pricing", "privacy", "platforms"],
        ticket: false,
    },
    Topic {
        id: "open_source",
        meaning: "open source, GitHub, source code, building it yourself",
        keywords: &["open source", "open-source", "github", "source code", "source", "build it myself", "self host", "self-host", "repo"],
        answer: "Yes. RamWarden is open source on GitHub, and you can build it yourself and configure your own model and security server. \
                 The plans on this site are the packaged, signed app with everything set up for you.",
        next: &["plans", "privacy"],
        ticket: false,
    },
    Topic {
        id: "my_account",
        meaning: "the visitor's own account: what plan they are on, their trial end date, their devices, their subscription status",
        keywords: &["my plan", "my account", "my subscription", "my trial", "am i on", "my devices", "when does my", "my status", "renew"],
        // Replaced by the account's real status when the visitor is signed
        // in; this is what a signed-out visitor sees.
        answer: "Sign in with the Account link at the bottom of the page and ask me again: I can read your own plan, trial date and computers (and nothing else).",
        next: &["signin", "devices"],
        ticket: true,
    },
    Topic {
        id: "human",
        meaning: "the visitor wants a person, a human, support, or reports a problem, bug, crash or something not working",
        keywords: &["human", "person", "someone", "real person", "support", "help me", "ticket", "contact", "email you", "bug", "broken", "not working", "doesn't work", "does not work", "error", "crash", "crashed", "failed", "problem", "issue"],
        answer: "I'll get a person on it. Open a ticket below, with an email if you'd like a reply there, and the team will answer. You'll also see the reply here when you come back.",
        next: &[],
        ticket: true,
    },
];

pub fn topic(id: &str) -> Option<&'static Topic> {
    TOPICS.iter().find(|t| t.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_topic_is_reachable_and_well_formed() {
        let mut seen = std::collections::HashSet::new();
        for t in TOPICS {
            assert!(seen.insert(t.id), "duplicate topic {}", t.id);
            assert!(!t.keywords.is_empty() && !t.meaning.is_empty(), "{}", t.id);
            assert!(t.keywords.iter().all(|k| *k == k.to_lowercase()), "{} keywords must be lowercase", t.id);
            for n in t.next {
                assert!(topic(n).is_some(), "{} links unknown topic {n}", t.id);
            }
            assert_ne!(chip(t.id), chip("nope"), "{} has no chip", t.id);
        }
        for w in ["ram", "threat"] {
            assert!(starters(w).iter().all(|s| topic(s).is_some()));
        }
    }

    #[test]
    fn copy_follows_the_site_rules() {
        let all = TOPICS.iter().map(|t| t.answer).chain([greeting("ram"), greeting("threat"), unknown_answer()]);
        for a in all {
            assert!(!a.contains('\u{2014}'), "no em dashes on the site: {a}");
            for name in ["Jev", "TypeSafe", "Wazuh", "ClamAV", "Suricata", "ntopng", "Claude", "Stripe"] {
                assert!(!a.contains(name), "no engine or provider names on the site ({name}): {a}");
            }
        }
    }

    /// The numbers a visitor will hold us to, against the plans the API sells.
    #[test]
    fn prices_and_limits_match_the_plans_sold() {
        let plans = crate::checkout::Plans::built_in();
        let usd = |tier: &str, interval: &str| {
            plans.plans.iter().find(|p| p.tier == tier && p.interval == interval).map(|p| p.usd).unwrap()
        };
        let pricing = topic("pricing").unwrap().answer;
        for (tier, name) in [("ram", "Plus"), ("pro", "Pro")] {
            let two = format!("{name} is ${} every 2 months or ${} a year", usd(tier, "2mo"), usd(tier, "year"));
            assert!(pricing.contains(&two), "pricing answer is stale: wanted `{two}`");
        }
        let devices = topic("devices").unwrap().answer;
        let want = format!(
            "Lite covers {} computer, Plus {} and Pro {}",
            crate::accounts::FREE_DEVICE_LIMIT,
            crate::accounts::PLUS_DEVICE_LIMIT,
            crate::accounts::PRO_DEVICE_LIMIT
        );
        assert!(devices.contains(&want), "devices answer is stale: wanted `{want}`");
    }
}
