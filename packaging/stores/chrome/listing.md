# Chrome Web Store listing — 0.3.3 candidate

Name: RamWarden
Category: Productivity
Language: English
Short description: Reversible idle-tab cleanup, with optional desktop RAM and security reports.

## Detailed description

For people who never close their tabs.

Give idle tabs a rest without losing your place. Turn on Browser Cleanup and
choose how many recent tabs to keep. When the desktop app is disconnected,
RamWarden unloads eligible tabs after an hour of inactivity. Tabs stay open and
reload when selected. Active, pinned, audible, private and protected work tabs
are excluded. Cleanup is opt-in and does not require an account or paid plan.

Connect the optional desktop app to view measured RAM usage and coordinate tab
cleanup. Closed tabs from the desktop workflow appear in a reopen list. Your
recent activity and work protections are checked before a tab is released.

The ThreatWarden pane displays the connected desktop app's security report,
engine health and recent alerts when available. A rule alert is not confirmed
malware. The extension itself is not antivirus software and does not install
security engines. Paid desktop features require the corresponding verified plan.

Manage optional desktop plans on the Warden website. Account sign-in and checkout
are separate: signing in with a wallet does not authorize a payment. Browser
Cleanup continues to work without buying a plan.

## Public URLs — must be live and verified before submitting

- Homepage: https://ramwarden.com/
- Privacy: https://ramwarden.com/privacy.html
- Setup/support: https://ramwarden.com/setup.html

Do not replace these with localhost, a parked domain or an unrelated product's
privacy policy. Public URL verification remains a release gate.

## Single purpose

RamWarden is the browser companion for reversible idle-tab cleanup and connected
computer health reporting. Optional desktop reports are provided by the same
RamWarden app; the extension does not independently scan files or network traffic.

## Permission justifications

- tabs: read URL/title and tab state to identify idle tabs, protect active work,
  unload eligible tabs, and reopen or close specific tabs in the desktop workflow.
- storage: retain local settings, activity timestamps, work-protection feedback,
  browser identifier and bounded reopen records. The daemon-host setting uses
  browser sync storage and may sync if Chrome Sync is enabled.
- alarms: schedule opted-in standalone cleanup and the bounded polling fallback.
- tabGroups: gather unloaded tabs in a reversible Idle group and remove a tab
  from that group when it is used again; leave user-created groups alone.
- localhost/127.0.0.1 hosts: communicate with the optional local desktop app.
- optional HTTP hosts: requested only when the user explicitly configures another
  trusted daemon address, such as a device on their own private network.
- HTTP/HTTPS content-script matches: observe trusted interaction timing and a
  boolean editing signal to protect work. The script does not read form values,
  passwords or page text. Broad page access is needed to protect work across
  normal websites, rather than only on a fixed list of sites.

## Privacy questionnaire guidance — publisher must verify exact selections

Disclose browsing history (tab URLs/titles) and user activity/interaction signals.
Do not claim “no data collected” or “nothing ever leaves the computer.” Tab
metadata is processed locally and can be sent to the configured desktop daemon.
Optional hosted analysis is governed by desktop settings and consent. The
extension stores no card data, private wallet keys or provider API credentials;
website authentication/payment processing is separate.

Data use is limited to the stated functionality, not sale, advertising or credit
scoring. Verify the current privacy policy and actual configured analysis path
before making the dashboard's Limited Use certifications. All executable code is
bundled in the package; no remote executable code is loaded.

## Reviewer walkthrough

1. Install the ZIP in a disposable Chrome profile. No desktop app or account is
   needed to inspect Browser Cleanup. It starts unchecked; enable it explicitly.
2. Set the recent-tab budget. Eligible tabs must have been idle for at least an
   hour; do not expect newly opened tabs to unload immediately. Active work stays.
3. Open ThreatWarden without a desktop app: it must say Protection unverified,
   not claim active protection. It must clear stale alerts after disconnect.
4. Connected desktop RAM/Threat reports require the separately installed app.
   Provide a real clean-machine installer and suitable reviewer access before
   asking reviewers to validate paid features. Never include operator tokens.
5. Wallet login and paid upgrades are explicit website actions. No payment is
   initiated automatically by this package.

## Artifacts

- Upload: dist/ramwarden-chrome-0.3.3.zip
- SHA-256: adjacent .zip.sha256 file
- Screenshots: generated with synthetic demonstration reports, 1280x800 PNG.
- Small promotional tile: 440x280 PNG. Optional marquee: 1400x560 PNG.

Status: package candidate only. No store item upload, review request or publication
has been performed by these packaging scripts.
