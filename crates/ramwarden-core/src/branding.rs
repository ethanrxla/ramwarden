//! Public capability names, separate from stable protocol identifiers and diagnostics.
pub fn public_text(input:&str)->String {
    let mut output=input.to_string();
    for (internal,public) in [("wazuh","Sentinel"),("clamav","Antivirus"),("freshclam","antivirus updater"),("suricata","Threat detection"),("nyquest","request optimizer")] {
        let mut cursor=0;
        while cursor+internal.len()<=output.len() {
            let found=output.as_bytes()[cursor..].windows(internal.len()).position(|bytes|bytes.eq_ignore_ascii_case(internal.as_bytes()));
            let Some(offset)=found else{break};let start=cursor+offset;let end=start+internal.len();
            output.replace_range(start..end,public);cursor=start+public.len();
        }
    }
    output
}
/// Service errors do not expose private deployment addresses.
pub fn service_detail(input:&str)->String {
    let mut text=public_text(input);
    while let Some(start)=text.find("http://").or_else(||text.find("https://")) {
        let rest=&text[start..];let end=rest.find(|c:char|c.is_whitespace() || matches!(c,')'|'"'|'\'')).unwrap_or(rest.len());
        text.replace_range(start..start+end,"service endpoint");
    }
    text
}

#[cfg(test)]mod tests {
    use super::*;
    #[test]fn capability_labels_hide_implementation_names_case_insensitively(){
        assert_eq!(public_text("WAZUH unavailable; ClamAV deferred; SURICATA dropped packets; Nyquest timeout"),"Sentinel unavailable; Antivirus deferred; Threat detection dropped packets; request optimizer timeout");
        assert_eq!(public_text("🐐 Wazuh: no feed"),"🐐 Sentinel: no feed");
        assert_eq!(service_detail("Wazuh request failed (https://internal.example:55000/token)"),"Sentinel request failed (service endpoint)");
    }
}

