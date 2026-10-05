//! What a process *is*, as distinct from what it is doing.
//!
//! # Why role comes before activity
//!
//! v1's original profiler classified purely by name, and anything it did not
//! recognise fell through to "idle" — which made a quiet 4 GB QEMU guest a
//! suspend candidate. The fix was to ask a different question first: not "is
//! this busy?" but "what would break if I froze it?"
//!
//! A virtual machine is protected because it is a virtual machine. It does not
//! matter that it is using no CPU — a stalled guest can corrupt its own disk.
//! Same for a container runtime (freezing the shim orphans everything inside),
//! a compositor (freezes the session), a terminal emulator (holds the user's
//! shells), and a build (interrupting it wastes real time).
//!
//! These tables are inherited from v1 essentially unchanged, because they encode
//! hard-won knowledge about this specific desktop. The one thing worth knowing
//! when editing them: the kernel truncates `comm` to 15 characters, so some
//! entries are deliberately spelled short — `power-profiles-`, `gnome-keyring-d`.
//!
//! Windows names live in their own tables below the Linux ones. They are
//! consulted on every platform — a name that never occurs on Linux costs a
//! comparison and nothing else — so classification has no `cfg` in it.

use std::fmt;

/// The kind of thing a process is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Role {
    /// Desktop compositor or session manager.
    Compositor,
    /// Virtual machine or emulator.
    Hypervisor,
    /// Container or sandbox runtime.
    Container,
    /// Long-running AI/coding agent.
    Agent,
    /// Terminal emulator — holds the user's shells.
    Terminal,
    /// System service, or anything running as root.
    System,
    /// Web browser. Deliberately *not* structural: browsers are the single
    /// largest consumer on this machine and reclaiming their cold memory is the
    /// main thing RamWarden exists to do.
    Browser,
    /// Playing or recording media.
    Media,
    /// A build or long computation in flight.
    Build,
    /// Background file sync or transfer.
    Sync,
    /// A game or its anti-cheat. Trimming a game's working set causes stutter,
    /// and anti-cheat services may treat a foreign process opening the game
    /// with memory rights as tampering.
    Game,
    /// An ordinary application. The only role, with [`Role::Browser`], that the
    /// ladder may reclaim from.
    App,
}

impl Role {
    /// Never suspend, never close, not even when explicitly asked.
    ///
    /// This is the guardrail that stops a hallucinated model suggestion or a
    /// fat-fingered watchlist entry from freezing the desktop.
    pub fn is_structural(self) -> bool {
        use Role::*;
        matches!(
            self,
            Compositor
                | Hypervisor
                | Container
                | Agent
                | Terminal
                | System
                | Media
                | Build
                | Sync
                | Game
        )
    }

    /// Why this role is off-limits, phrased for the user rather than the log.
    pub fn protection_reason(self) -> &'static str {
        use Role::*;
        match self {
            Compositor => "desktop compositor — suspending it freezes the session",
            Hypervisor => "virtual machine — a stalled guest can corrupt its disk",
            Container => "container runtime — freezing it orphans everything inside",
            Agent => "AI agent session in progress",
            Terminal => "terminal emulator — holds your shells",
            System => "system service",
            Media => "playing or recording media",
            Build => "build or long computation in flight",
            Sync => "file sync or transfer in progress",
            Game => "game or anti-cheat in progress — touching it causes stutter and may be flagged",
            Browser => "browser",
            App => "application",
        }
    }

    pub fn as_str(self) -> &'static str {
        use Role::*;
        match self {
            Compositor => "COMPOSITOR",
            Hypervisor => "HYPERVISOR",
            Container => "CONTAINER",
            Agent => "AGENT",
            Terminal => "TERMINAL",
            System => "SYSTEM",
            Browser => "BROWSER",
            Media => "MEDIA",
            Build => "BUILD",
            Sync => "SYNC",
            Game => "GAME",
            App => "APP",
        }
    }
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

const COMPOSITOR: &[&str] = &[
    "cosmic-comp", "cosmic-session", "cosmic-panel", "cosmic-applets",
    "mutter", "kwin_wayland", "kwin_x11", "xfwm4", "openbox", "i3", "sway",
    "xorg", "x", "xwayland", "gnome-shell", "plasmashell",
    "cosmic-greeter", "gdm", "sddm", "lightdm",
];
const COMPOSITOR_PREFIXES: &[&str] = &["cosmic-", "xdg-desktop-portal", "gnome-shell", "plasma"];

/// A SIGSTOP here stalls a guest OS mid-write.
const HYPERVISOR_PREFIXES: &[&str] = &[
    "qemu", "kvm", "virtualbox", "vboxheadless", "vboxsvc",
    "vmware", "virt-manager", "libvirtd", "virtiofsd",
    "crosvm", "cloud-hypervisor", "waydroid",
];

/// Freezing the shim orphans everything inside it.
const CONTAINER_PREFIXES: &[&str] = &[
    "containerd", "dockerd", "docker-proxy", "docker",
    "podman", "conmon", "crun", "runc", "systemd-nspawn", "lxc", "lxd",
    "bwrap", "flatpak-session-helper", "snapd",
];

const AGENT_NAMES: &[&str] = &["claude", "codex", "ollama", "aider", "cursor-agent", "copilot"];
const AGENT_CMDLINE: &[&str] = &["claude-code", "ollama serve"];

const TERMINAL_EMULATORS: &[&str] = &[
    "cosmic-term", "gnome-terminal", "gnome-terminal-server", "tilix", "xterm",
    "alacritty", "kitty", "konsole", "xfce4-terminal", "lxterminal",
    "mate-terminal", "terminator", "wezterm", "wezterm-gui", "foot", "ptyxis",
];

const BROWSERS: &[&str] = &[
    "brave", "brave-browser", "chrome", "chromium", "chromium-browser",
    "google-chrome", "firefox", "firefox-bin", "librewolf", "vivaldi",
    "epiphany", "tor", "torbrowser-launch",
];
const BROWSER_PREFIXES: &[&str] = &["brave", "firefox", "chrom"];

/// The user is watching, listening, or recording.
const MEDIA: &[&str] = &[
    "mpv", "vlc", "mplayer", "ffmpeg", "ffplay", "obs", "obs-studio",
    "totem", "celluloid", "audacity", "kdenlive", "spotify", "rhythmbox",
];

/// Interrupting any of these wastes the user's time.
const BUILD: &[&str] = &[
    "cargo", "rustc", "gcc", "cc1", "cc1plus", "g++", "clang", "clang++",
    "ld", "make", "ninja", "cmake", "gradle", "javac", "kotlinc", "go",
    "webpack", "tsc", "esbuild", "vite", "rollup", "pytest", "tox",
    "dpkg", "apt", "apt-get", "unattended-upgr", "snap", "flatpak",
];

/// Suspending mid-transfer can corrupt remote state.
const SYNC: &[&str] = &[
    "syncthing", "dropbox", "nextcloud", "insync", "rclone", "rsync",
    "megasync", "onedrive", "restic", "borg", "duplicity", "timeshift",
];

const SYSTEM_NAMES: &[&str] = &[
    "systemd", "init", "kthreadd", "pipewire", "pipewire-pulse", "wireplumber",
    "pulseaudio", "dbus-daemon", "dbus-broker", "networkmanager",
    "wpa_supplicant", "avahi-daemon", "polkitd", "udisksd", "upowerd",
    "bluetoothd", "cupsd", "gvfsd", "gnome-keyring-d", "gnome-keyring-daemon",
    "systemd-resolved", "systemd-udevd", "systemd-journald", "systemd-logind",
    "tailscaled", "wireguard", "openvpn", "sshd", "cron", "crond", "atd",
    "accounts-daemon", "rtkit-daemon", "irqbalance", "thermald", "power-profiles-",
];

// ── Windows ─────────────────────────────────────────────────────────────────
// Image names without `.exe`, compared lowercased.

/// The desktop shell and the pieces of it that freeze the session if stopped.
const WIN_SHELL: &[&str] = &[
    "dwm", "explorer", "sihost", "shellexperiencehost", "startmenuexperiencehost",
    "searchhost", "textinputhost", "applicationframehost", "lockapp", "ctfmon",
    "fontdrvhost", "logonui",
];

/// Core system processes and the security stack. Most run in session 0 and are
/// already classified as system by owner; these are the ones that also run in
/// the user's session, or must never depend on owner lookup succeeding.
const WIN_SYSTEM: &[&str] = &[
    "system", "idle", "registry", "secure system", "memory compression", "smss", "csrss",
    "wininit", "winlogon", "services", "lsass", "lsaiso", "svchost", "taskhostw",
    "runtimebroker", "dllhost", "audiodg", "spoolsv", "wudfhost", "wmiprvse",
    "msmpeng", "nissrv", "mpdefendercoreservice", "securityhealthservice",
    "securityhealthsystray", "smartscreen", "nvcontainer", "nvdisplay.container",
];

/// Hyper-V and WSL2 guests live in these.
const WIN_HYPERVISOR_PREFIXES: &[&str] = &["vmmem", "vmwp", "vmcompute", "virtualboxvm"];

/// Docker Desktop and the WSL plumbing a running distro depends on.
const WIN_CONTAINER_PREFIXES: &[&str] = &["com.docker", "docker desktop", "wslservice", "wslhost", "wslrelay"];

const WIN_TERMINALS: &[&str] = &["windowsterminal", "openconsole", "conhost"];

const WIN_BROWSERS: &[&str] = &["msedge", "opera", "operagx"];

const WIN_MEDIA: &[&str] = &["obs64", "obs32", "mpc-hc64", "potplayermini64"];

/// Compilers, and installers mid-flight: interrupting Windows Update or an MSI
/// can leave the system half-patched.
const WIN_BUILD: &[&str] = &[
    "cl", "link", "msbuild", "mspdbsrv", "vctip", "nvcc", "msiexec", "trustedinstaller",
    "tiworker", "wuauclt", "wuaucltcore", "mousocoreworker", "usoclient",
];

const WIN_SYNC: &[&str] = &["googledrivefs", "icloudservices"];

/// Anti-cheat services and clients. Never opened, never trimmed.
const ANTI_CHEAT: &[&str] = &[
    "vgc", "vgtray", "easyanticheat", "easyanticheat_eos", "beservice",
    "eaanticheat.gameservice", "faceit", "faceitservice", "gameguard",
];

/// Games recognised by name. Most Unreal titles are also caught by the
/// `-shipping` suffix their builds carry.
const GAMES: &[&str] = &[
    "league of legends", "valorant", "cs2", "dota2", "rocketleague", "r5apex",
    "overwatch", "eldenring", "gta5", "gtav", "minecraft", "javaw",
];

fn is_game(lower: &str) -> bool {
    lower.ends_with("-shipping")
        || GAMES.contains(&lower)
        || ANTI_CHEAT.contains(&lower)
}

fn starts_with_any(s: &str, prefixes: &[&str]) -> bool {
    prefixes.iter().any(|p| s.starts_with(p))
}

/// Classify a process from its name, command line, and owning UID.
///
/// `uid` may be `None` when unreadable; pass `Some(0)` to have root-owned
/// processes classified as [`Role::System`], which is what v1 did.
pub fn classify(name: &str, cmdline: &str, uid: Option<u32>) -> Role {
    let lower = name.trim().to_lowercase();
    let cmd = cmdline.to_lowercase();

    // Exact names beat prefixes, so `cosmic-term` is a terminal rather than
    // being swallowed by the `cosmic-` compositor prefix. Order is load-bearing.
    let name = lower.as_str();
    if TERMINAL_EMULATORS.contains(&name) || WIN_TERMINALS.contains(&name) {
        return Role::Terminal;
    }
    if COMPOSITOR.contains(&name)
        || starts_with_any(&lower, COMPOSITOR_PREFIXES)
        || WIN_SHELL.contains(&name)
    {
        return Role::Compositor;
    }
    if starts_with_any(&lower, HYPERVISOR_PREFIXES) || starts_with_any(&lower, WIN_HYPERVISOR_PREFIXES) {
        return Role::Hypervisor;
    }
    if starts_with_any(&lower, CONTAINER_PREFIXES) || starts_with_any(&lower, WIN_CONTAINER_PREFIXES) {
        return Role::Container;
    }
    // Before agents and browsers: a game is never something to reclaim from,
    // whatever else its name resembles.
    if is_game(name) {
        return Role::Game;
    }
    if AGENT_NAMES.contains(&lower.as_str()) || AGENT_CMDLINE.iter().any(|a| cmd.contains(a)) {
        return Role::Agent;
    }
    if BROWSERS.contains(&name) || starts_with_any(&lower, BROWSER_PREFIXES) || WIN_BROWSERS.contains(&name) {
        return Role::Browser;
    }
    if MEDIA.contains(&name) || WIN_MEDIA.contains(&name) {
        return Role::Media;
    }
    if BUILD.contains(&name) || WIN_BUILD.contains(&name) {
        return Role::Build;
    }
    if SYNC.contains(&name) || WIN_SYNC.contains(&name) {
        return Role::Sync;
    }
    if SYSTEM_NAMES.contains(&name) || WIN_SYSTEM.contains(&name) || uid == Some(0) {
        return Role::System;
    }
    Role::App
}

#[cfg(test)]
mod tests {
    use super::*;

    fn role_of(name: &str) -> Role {
        classify(name, "", None)
    }

    /// Ported verbatim from v1's `test_structural_roles_are_recognised`.
    #[test]
    fn structural_roles_are_recognised() {
        for (name, expected) in [
            ("qemu-system-x86_64", Role::Hypervisor),
            ("VirtualBoxVM", Role::Hypervisor),
            ("containerd-shim-runc-v2", Role::Container),
            ("dockerd", Role::Container),
            ("cosmic-comp", Role::Compositor),
            ("gnome-shell", Role::Compositor),
            ("claude", Role::Agent),
            ("cosmic-term", Role::Terminal),
            ("ffmpeg", Role::Media),
            ("cargo", Role::Build),
            ("syncthing", Role::Sync),
        ] {
            assert_eq!(role_of(name), expected, "{name}");
            assert!(expected.is_structural(), "{name} must be protected");
        }
    }

    /// `cosmic-term` would be caught by the `cosmic-` compositor prefix if the
    /// exact-name check did not run first. Getting this wrong protects the
    /// terminal for the wrong reason and, worse, suggests the compositor is a
    /// terminal.
    #[test]
    fn exact_names_beat_prefixes() {
        assert_eq!(role_of("cosmic-term"), Role::Terminal);
        assert_eq!(role_of("cosmic-comp"), Role::Compositor);
        assert_eq!(role_of("cosmic-panel"), Role::Compositor);
        assert_eq!(role_of("cosmic-launcher"), Role::Compositor);
    }

    #[test]
    fn classification_is_case_insensitive() {
        assert_eq!(role_of("DOCKERD"), Role::Container);
        assert_eq!(role_of("VirtualBoxVM"), Role::Hypervisor);
        assert_eq!(role_of("Cosmic-Comp"), Role::Compositor);
    }

    #[test]
    fn browsers_are_identified_but_are_not_structural() {
        for name in ["brave", "firefox", "chromium", "brave-browser", "chrome"] {
            assert_eq!(role_of(name), Role::Browser, "{name}");
        }
        assert!(
            !Role::Browser.is_structural(),
            "reclaiming browser memory is the point of RamWarden"
        );
    }

    #[test]
    fn an_unknown_process_is_an_ordinary_app() {
        assert_eq!(role_of("some-random-thing"), Role::App);
        assert!(!Role::App.is_structural());
    }

    /// The regression that motivated dynamic detection: an unrecognised name
    /// must not be the only thing standing between a VM and a SIGSTOP. Here the
    /// name *is* recognised, and the role alone protects it.
    #[test]
    fn a_quiet_virtual_machine_is_protected_by_its_role_alone() {
        let r = role_of("qemu-system-x86_64");
        assert_eq!(r, Role::Hypervisor);
        assert!(r.is_structural());
        assert!(r.protection_reason().contains("virtual machine"));
    }

    #[test]
    fn root_owned_processes_are_system_services() {
        assert_eq!(classify("some-daemon", "", Some(0)), Role::System);
        assert_eq!(classify("some-daemon", "", Some(1000)), Role::App);
        assert_eq!(classify("some-daemon", "", None), Role::App);
    }

    #[test]
    fn an_agent_is_recognised_from_its_command_line() {
        assert_eq!(classify("node", "/usr/bin/claude-code --resume", None), Role::Agent);
        assert_eq!(classify("sh", "ollama serve", None), Role::Agent);
        // `ollama list` exits in a moment and is not a session to protect...
        // but the bare name is on the agent list, so it is protected anyway.
        assert_eq!(classify("ollama", "ollama list", None), Role::Agent);
    }

    /// The kernel truncates `comm` to 15 characters, which is why some table
    /// entries are spelled short. Changing them to the full name would silently
    /// stop matching.
    #[test]
    fn truncated_comm_names_are_spelled_as_the_kernel_reports_them() {
        assert_eq!(role_of("power-profiles-"), Role::System);
        assert_eq!(role_of("gnome-keyring-d"), Role::System);
        assert_eq!(role_of("unattended-upgr"), Role::Build);
        // containerd-shim-runc-v2 arrives truncated, and the prefix still catches it.
        assert_eq!(role_of("containerd-shi"), Role::Container);
    }

    #[test]
    fn every_role_has_a_reason_the_user_can_read() {
        for r in [
            Role::Compositor, Role::Hypervisor, Role::Container, Role::Agent,
            Role::Terminal, Role::System, Role::Browser, Role::Media,
            Role::Build, Role::Sync, Role::Game, Role::App,
        ] {
            let reason = r.protection_reason();
            assert!(reason.len() > 5, "{r}: {reason:?} is too terse to be useful");
        }
    }

    /// A protected process shows the user *why*, and for the structural roles
    /// that means naming the consequence — "freezes the session", not just
    /// "compositor". This is what stops the UI reading as an arbitrary refusal.
    #[test]
    fn structural_reasons_name_a_consequence() {
        for r in [Role::Compositor, Role::Hypervisor, Role::Container, Role::Terminal, Role::Game] {
            let reason = r.protection_reason();
            assert!(
                reason.contains('—') || reason.contains("in progress"),
                "{r}: {reason:?} states what it is but not what breaks"
            );
        }
    }

    #[test]
    fn only_reclaimable_roles_are_non_structural() {
        assert!(!Role::App.is_structural());
        assert!(!Role::Browser.is_structural());
        for r in [
            Role::Compositor, Role::Hypervisor, Role::Container, Role::Agent,
            Role::Terminal, Role::System, Role::Media, Role::Build, Role::Sync, Role::Game,
        ] {
            assert!(r.is_structural(), "{r}");
        }
    }

    // ── Windows ─────────────────────────────────────────────────────────────

    /// Image names exactly as `ramwarden-nt` reports them: without `.exe`,
    /// case preserved.
    #[test]
    fn windows_structural_processes_are_recognised() {
        for (name, expected) in [
            ("dwm", Role::Compositor),
            ("explorer", Role::Compositor),
            ("StartMenuExperienceHost", Role::Compositor),
            ("csrss", Role::System),
            ("lsass", Role::System),
            ("MsMpEng", Role::System),
            ("svchost", Role::System),
            ("Memory Compression", Role::System),
            ("vmmemWSL", Role::Hypervisor),
            ("vmmem", Role::Hypervisor),
            ("com.docker.backend", Role::Container),
            ("wslservice", Role::Container),
            ("WindowsTerminal", Role::Terminal),
            ("OpenConsole", Role::Terminal),
            ("wuaucltcore", Role::Build),
            ("TiWorker", Role::Build),
            ("OneDrive", Role::Sync),
            ("obs64", Role::Media),
        ] {
            assert_eq!(role_of(name), expected, "{name}");
            assert!(expected.is_structural(), "{name} must be protected");
        }
    }

    /// Measured on the target machine with Fortnite running: the game, its
    /// overlay and its anti-cheat must all be untouchable.
    #[test]
    fn games_and_anti_cheat_are_never_reclaimable() {
        for name in [
            "FortniteClient-Win64-Shipping",
            "EOSOverlayRenderer-Win64-Shipping",
            "VALORANT-Win64-Shipping",
            "EasyAntiCheat_EOS",
            "vgc",
            "League of Legends",
            "cs2",
        ] {
            assert_eq!(role_of(name), Role::Game, "{name}");
        }
        assert!(Role::Game.is_structural());
    }

    /// The apps RamWarden exists to reclaim from on Windows stay reclaimable.
    #[test]
    fn windows_apps_and_browsers_stay_reclaimable() {
        for (name, expected) in [
            ("brave", Role::Browser),
            ("msedge", Role::Browser),
            ("chrome", Role::Browser),
            ("Discord", Role::App),
            ("LM Studio", Role::App),
            ("EpicGamesLauncher", Role::App),
            ("RiotClientServices", Role::App),
            ("msedgewebview2", Role::App),
        ] {
            assert_eq!(role_of(name), expected, "{name}");
            assert!(!expected.is_structural(), "{name}");
        }
    }

    #[test]
    fn role_names_round_trip_as_the_api_strings_v1_emitted() {
        assert_eq!(Role::Hypervisor.to_string(), "HYPERVISOR");
        assert_eq!(Role::App.as_str(), "APP");
    }
}
