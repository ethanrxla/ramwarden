//! Bounded decoding of the daemon's single-line JSON SSE workspace events.
#[derive(Default)]
pub(crate) struct Decoder {
    pending: Vec<u8>,
    total: usize,
}
impl Decoder {
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<serde_json::Value>, String> {
        self.total += bytes.len();
        if self.total > 256 * 1024 { return Err("Workspace stream exceeded limit".into()); }
        self.pending.extend_from_slice(bytes);
        let mut events = vec![];
        while let Some(end) = self.pending.iter().position(|b| *b == b'\n') {
            let line: Vec<_> = self.pending.drain(..=end).collect();
            if let Some(data) = line.strip_prefix(b"data:") {
                events.push(serde_json::from_slice(data).map_err(|_| "Invalid workspace event")?);
            }
        }
        if self.pending.len() > 64 * 1024 { return Err("Workspace event exceeded limit".into()); }
        Ok(events)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn arbitrary_network_splits_preserve_unicode_and_multiple_events() {
        let bytes = "event: workspace\r\ndata: {\"kind\":\"result\",\"text\":\"é\"}\r\n\r\ndata: {\"kind\":\"finished\"}\n\n".as_bytes();
        for split in 0..bytes.len() {
            let mut d = Decoder::default();
            let mut events = d.push(&bytes[..split]).unwrap();
            events.extend(d.push(&bytes[split..]).unwrap());
            assert_eq!(events.len(), 2);
            assert_eq!(events[0]["text"], "é");
            assert_eq!(events[1]["kind"], "finished");
        }
    }
    #[test]
    fn malformed_and_unbounded_streams_are_refused() {
        assert!(Decoder::default().push(b"data: not json\n").is_err());
        assert!(Decoder::default().push(&vec![b'x'; 65537]).is_err());
        assert!(Decoder::default().push(&vec![b'\n'; 262145]).is_err());
    }
}
