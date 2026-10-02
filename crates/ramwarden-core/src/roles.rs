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
            Compositor | Hypervisor | Container | Agent | Terminal | System | Media | Build | Sync
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
    if TERMINAL_EMULATORS.contains(&lower.as_str()) {
        return Role::Terminal;
    }
    if COMPOSITOR.contains(&lower.as_str()) || starts_with_any(&lower, COMPOSITOR_PREFIXES) {
        return Role::Compositor;
    }
    if starts_with_any(&lower, HYPERVISOR_PREFIXES) {
        return Role::Hypervisor;
    }
    if starts_with_any(&lower, CONTAINER_PREFIXES) {
        return Role::Container;
    }
    if AGENT_NAMES.contains(&lower.as_str()) || AGENT_CMDLINE.iter().any(|a| cmd.contains(a)) {
        return Role::Agent;
    }
    if BROWSERS.contains(&lower.as_str()) || starts_with_any(&lower, BROWSER_PREFIXES) {
        return Role::Browser;
    }
    if MEDIA.contains(&lower.as_str()) {
        return Role::Media;
    }
    if BUILD.contains(&lower.as_str()) {
        return Role::Build;
    }
    if SYNC.contains(&lower.as_str()) {
        return Role::Sync;
    }
    if SYSTEM_NAMES.contains(&lower.as_str()) || uid == Some(0) {
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
            Role::Build, Role::Sync, Role::App,
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
        for r in [Role::Compositor, Role::Hypervisor, Role::Container, Role::Terminal] {
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
            Role::Terminal, Role::System, Role::Media, Role::Build, Role::Sync,
        ] {
            assert!(r.is_structural(), "{r}");
        }
    }

    #[test]
    fn role_names_round_trip_as_the_api_strings_v1_emitted() {
        assert_eq!(Role::Hypervisor.to_string(), "HYPERVISOR");
        assert_eq!(Role::App.as_str(), "APP");
    }
}
