//! Only known layout settings may change. Searching added lines for `exec`
//! misses multiline strings, escaped keys, includes and variables used by
//! existing launch commands. Preserve everything outside the editable subset.
use ramwarden_platform::WmKind;

pub(super) fn check(kind: WmKind, old: &str, new: &str) -> Result<(), String> {
    let safe = match kind {
        WmKind::AeroSpace => aerospace(old, new),
        WmKind::Komorebi => komorebi(old, new),
        WmKind::Sway | WmKind::I3 => ipc(old, new),
    };
    if safe {
        Ok(())
    } else {
        Err("config change dropped: only layout and gap settings can be edited; a change that starts a program or alters other config must be made by hand".into())
    }
}

fn aerospace(old: &str, new: &str) -> bool {
    let (Ok(mut a), Ok(mut b)) = (old.parse::<toml::Table>(), new.parse::<toml::Table>()) else {
        return false;
    };
    for key in [
        "gaps",
        "accordion-padding",
        "default-root-container-layout",
        "default-root-container-orientation",
        "enable-normalization-flatten-containers",
        "enable-normalization-opposite-orientation-for-nested-containers",
    ] {
        a.remove(key);
        b.remove(key);
    }
    a == b
}

fn komorebi(old: &str, new: &str) -> bool {
    let parse = |s: &str| serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(s);
    let (Ok(mut a), Ok(mut b)) = (parse(old), parse(new)) else {
        return false;
    };
    for key in [
        "default_workspace_padding",
        "default_container_padding",
        "border_width",
        "border_offset",
    ] {
        a.remove(key);
        b.remove(key);
    }
    a == b
}

// Keep all other lines byte-for-byte and in order, including comments, blocks,
// variable definitions and include directives. Only simple numeric gaps are
// recognised; command separators and continuation lines cannot pass this gate.
fn ipc(old: &str, new: &str) -> bool {
    fn fixed(text: &str) -> Vec<&str> {
        text.lines().filter(|line| {
            let parts: Vec<_> = line.split_whitespace().collect();
            !matches!(parts.as_slice(), ["gaps", scope, value]
                if ["inner", "outer", "horizontal", "vertical", "top", "right", "bottom", "left"].contains(scope)
                    && value.parse::<u32>().is_ok())
        }).collect()
    }
    // A preceding continuation can make an apparent gap line part of exec.
    !old.lines()
        .chain(new.lines())
        .any(|l| l.trim_end().ends_with('\\'))
        && fixed(old) == fixed(new)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multiline_launch_edits_and_escaped_launch_keys_are_refused() {
        let old = "after-startup-command = [\"\"\"exec-and-forget\nold-command\"\"\"]\ngaps.inner.horizontal = 4\n";
        assert!(
            check(
                WmKind::AeroSpace,
                old,
                &old.replace("old-command", "new-command")
            )
            .is_err()
        );
        assert!(
            check(
                WmKind::AeroSpace,
                "",
                "\"after-startup-comman\\u0064\" = ['exec-and-forget something']"
            )
            .is_err()
        );
        assert!(
            check(
                WmKind::AeroSpace,
                old,
                &old.replace("horizontal = 4", "horizontal = 8")
            )
            .is_ok()
        );
    }

    #[test]
    fn includes_variables_and_command_continuations_are_refused() {
        for kind in [WmKind::Sway, WmKind::I3] {
            assert!(check(kind, "gaps inner 4\n", "gaps inner 8\n").is_ok());
            assert!(check(kind, "gaps inner 4\n", "include /tmp/other\ngaps inner 8\n").is_err());
            assert!(
                check(
                    kind,
                    "set $app old\nexec $app\n",
                    "set $app new\nexec $app\n"
                )
                .is_err()
            );
            assert!(
                check(
                    kind,
                    "exec echo \\\ngaps inner 4\n",
                    "exec echo \\\ngaps inner 8\n"
                )
                .is_err()
            );
        }
    }

    #[test]
    fn komorebi_hooks_cannot_change_but_padding_can() {
        let old = r#"{"default_container_padding":4,"command":"old"}"#;
        assert!(check(WmKind::Komorebi, old, &old.replace(":4", ":8")).is_ok());
        assert!(check(WmKind::Komorebi, old, &old.replace("old", "new")).is_err());
    }
}
