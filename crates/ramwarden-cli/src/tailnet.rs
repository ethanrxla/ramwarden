use super::*;
#[derive(Debug)]
pub struct Peer {
    name: String,
    ip: std::net::IpAddr,
    online: bool,
}
fn clean(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).take(180).collect()
}
fn peers(value: &Value) -> Vec<Peer> {
    let mut rows: Vec<Peer> = value["Peer"]
        .as_object()
        .into_iter()
        .flat_map(|m| m.values())
        .filter_map(|p| {
            let ip = p["TailscaleIPs"]
                .as_array()?
                .iter()
                .filter_map(|v| v.as_str()?.parse().ok())
                .find(|ip: &std::net::IpAddr| ip.is_ipv4())?;
            Some(Peer {
                name: clean(p["HostName"].as_str().unwrap_or("Computer")),
                ip,
                online: p["Online"].as_bool().unwrap_or(false),
            })
        })
        .collect();
    rows.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.ip.cmp(&b.ip)));
    rows
}
async fn tailscale_output(path: &str) -> Result<std::process::Output> {
    let mut command = tokio::process::Command::new(path);
    command.args(["status", "--json"]).kill_on_drop(true);
    Ok(tokio::time::timeout(Duration::from_secs(3), command.output()).await??)
}
pub async fn inventory() -> Result<Vec<Peer>> {
    let output = match tailscale_output("tailscale").await {
        Ok(output) => output,
        Err(_) if cfg!(target_os = "macos") => {
            tailscale_output("/Applications/Tailscale.app/Contents/MacOS/Tailscale").await?
        }
        Err(error) => return Err(error),
    };
    if !output.status.success() {
        return Err("Tailscale is not connected. Sign into your own tailnet first".into());
    }
    let value: Value = serde_json::from_slice(&output.stdout)?;
    if value["BackendState"].as_str() != Some("Running") {
        return Err("Tailscale is not running".into());
    }
    Ok(peers(&value))
}
fn selection(input: &str, total: usize, remaining: usize) -> Result<Vec<usize>> {
    let mut picked = Vec::new();
    for item in input.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let n = item
            .parse::<usize>()
            .map_err(|_| "Choose machine numbers, separated by commas")?;
        if n == 0 || n > total {
            return Err("Machine number is out of range".into());
        }
        if !picked.contains(&(n - 1)) {
            picked.push(n - 1);
        }
    }
    if picked.len() > remaining {
        return Err("Selection exceeds your remaining account device allowance".into());
    }
    Ok(picked)
}
pub async fn select(http: &reqwest::Client) -> Result<()> {
    banner();
    println!(
        "Your tailnet · optional remote setup\nNo tailnet joins, SSH changes or remote installs happen here."
    );
    let a = account(http).await?;
    let limit = a["device_limit"]
        .as_u64()
        .ok_or("Missing verified account limit")? as usize;
    let used = a["devices_used"]
        .as_u64()
        .ok_or("Missing verified device count")? as usize;
    let remaining = limit.saturating_sub(used);
    println!(
        "{}: {used}/{limit} connected · up to {remaining} new computers",
        a["plan"].as_str().unwrap_or("Account")
    );
    let rows = inventory().await?;
    for (i, p) in rows.iter().enumerate() {
        println!(
            "  {}. {} · {} · {}",
            i + 1,
            p.name,
            p.ip,
            if p.online { "online" } else { "offline" }
        );
    }
    if rows.is_empty() || remaining == 0 {
        println!("No new devices can be selected. Manage existing computers on your account page.");
        return Ok(());
    }
    if !io::stdin().is_terminal() {
        println!("Run interactively to select machines. Nothing was changed.");
        return Ok(());
    }
    let picked = selection(
        &prompt("Choose new machines (for example 1,3), or Enter to skip:")?,
        rows.len(),
        remaining,
    )?;
    if picked.is_empty() {
        return Ok(());
    }
    println!(
        "\nCopy this request to your agent:\n\nSet up RamWarden on ONLY these selected machines in my existing Tailscale tailnet:"
    );
    for i in picked {
        println!(
            "- {} ({}){}",
            rows[i].name,
            rows[i].ip,
            if rows[i].online {
                ""
            } else {
                " [currently offline; wait until reachable]"
            }
        );
    }
    println!(
        "\nFirst inspect the existing installation and run ramwarden doctor. Preserve settings and credentials. Reuse an already-enrolled device instead of consuming a new seat. My current limit is {limit} computers, with {remaining} new seats available; recheck it before each enrollment. Use existing authorized SSH/Tailscale access; never enable SSH, change tailnet rules, or join another tailnet without my approval. Use the official installer at https://ramwarden.com/setup.html and run ramwarden setup. Let me approve browser sign-in and any administrator prompt. Do not copy account cookies or device tokens between machines. Verify each device's RAM readings and ThreatWarden engine health separately; selecting a machine or having a Pro pass does not prove protection. Stop and report missing hosted services. Never claim coverage without measurements.\n\nNo remote commands have been run. Each machine still needs its own authorized installation and enrollment."
    );
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_deduplicated_selection() {
        assert_eq!(selection("1,1,3", 3, 2).unwrap(), vec![0, 2]);
        assert!(selection("0", 3, 3).is_err());
        assert!(selection("4", 3, 3).is_err());
        assert!(selection("1,2,3", 3, 2).is_err());
        assert!(selection("1; touch /tmp/no", 3, 3).is_err());
    }
    #[test]
    fn inventory_excludes_self_and_sanitizes_terminal_controls() {
        let p = peers(
            &serde_json::json!({"Self":{"HostName":"self"},"Peer":{"a":{"HostName":"bad\u{1b}name","TailscaleIPs":["100.64.0.2"],"Online":true},"b":{"HostName":"invalid","TailscaleIPs":["not an IP"]}}}),
        );
        assert_eq!(p.len(), 1);
        assert!(!p[0].name.contains('\u{1b}'));
    }
}
