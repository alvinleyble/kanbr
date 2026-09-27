//! A passphrase held only in memory, for the one request it belongs to.
//!
//! Kanbr never stores a passphrase: it lives in a [`Secret`] from the masked
//! prompt until the request carrying it is written to Firstmate, and its bytes
//! (including any spare capacity) are overwritten with zeros when it is
//! dropped. It has no `Display`, and its `Debug` never shows the text.

use std::fmt;
use std::hint::black_box;

pub struct Secret(Vec<u8>);

impl Secret {
    pub fn new() -> Self {
        Secret(Vec::with_capacity(128))
    }

    /// Appends a character without leaving an unwiped copy behind when the
    /// buffer has to grow.
    pub fn push(&mut self, c: char) {
        let mut buf = [0u8; 4];
        self.push_bytes(c.encode_utf8(&mut buf).as_bytes());
        buf.fill(0);
        black_box(&buf);
    }

    pub fn push_str(&mut self, s: &str) {
        self.push_bytes(s.as_bytes());
    }

    fn push_bytes(&mut self, bytes: &[u8]) {
        if self.0.len() + bytes.len() > self.0.capacity() {
            let mut grown = Vec::with_capacity((self.0.len() + bytes.len()).max(64) * 2);
            grown.extend_from_slice(&self.0);
            let old = std::mem::replace(&mut self.0, grown);
            drop(Secret(old));
        }
        self.0.extend_from_slice(bytes);
    }

    pub fn pop(&mut self) {
        let Ok(text) = std::str::from_utf8(&self.0) else {
            self.0.clear();
            return;
        };
        if let Some(c) = text.chars().next_back() {
            let keep = self.0.len() - c.len_utf8();
            for b in &mut self.0[keep..] {
                *b = 0;
            }
            self.0.truncate(keep);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The number of characters, for the masked prompt.
    pub fn chars(&self) -> usize {
        std::str::from_utf8(&self.0).map_or(0, |s| s.chars().count())
    }

    pub fn expose(&self) -> &str {
        std::str::from_utf8(&self.0).unwrap_or("")
    }

    pub fn bytes(&self) -> &[u8] {
        &self.0
    }
}

impl Default for Secret {
    fn default() -> Self {
        Secret::new()
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        let cap = self.0.capacity();
        self.0.clear();
        self.0.resize(cap, 0);
        black_box(&self.0);
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(hidden)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_and_never_prints() {
        let mut s = Secret::new();
        for c in "wörd".chars() {
            s.push(c);
        }
        assert_eq!(s.expose(), "wörd");
        assert_eq!(s.chars(), 4);
        s.pop();
        assert_eq!(s.expose(), "wör");
        s.pop();
        assert_eq!(s.expose(), "wö");
        assert_eq!(format!("{s:?}"), "Secret(hidden)");
        let mut long = Secret::new();
        for _ in 0..500 {
            long.push('x');
        }
        assert_eq!(long.chars(), 500);
        s.pop();
        s.pop();
        s.pop();
        assert!(s.is_empty());
    }
}
