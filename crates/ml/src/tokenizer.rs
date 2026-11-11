//! CLIP's byte-level BPE tokenizer, read from a Hugging Face `tokenizer.json`.
//!
//! Written by hand because the `tokenizers` crate is large and CLIP needs only this:
//! lowercase, split into words, map bytes to printable characters, merge pairs by rank.

use std::collections::HashMap;
use std::path::Path;

use crate::{Error, Result};

pub const START: i64 = 49406;
pub const END: i64 = 49407;
/// CLIP's text model reads at most this many tokens, including START and END.
pub const MAX_TOKENS: usize = 77;

pub struct Tokenizer {
    vocab: HashMap<String, i64>,
    /// Merge rank of a pair `(left, right)`; lower merges first.
    ranks: HashMap<(String, String), usize>,
    /// Byte to printable character, as in GPT-2/CLIP.
    byte_chars: [char; 256],
}

impl Tokenizer {
    pub fn open(path: &Path) -> Result<Tokenizer> {
        let text = std::fs::read_to_string(path)?;
        let json: serde_json::Value = serde_json::from_str(&text).map_err(|e| Error::Model(format!("{}: {e}", path.display())))?;
        let model = &json["model"];
        let bad = || Error::Model(format!("{}: not a BPE tokenizer", path.display()));
        let vocab: HashMap<String, i64> = model["vocab"]
            .as_object()
            .ok_or_else(bad)?
            .iter()
            .filter_map(|(k, v)| Some((k.clone(), v.as_i64()?)))
            .collect();
        let mut ranks = HashMap::new();
        for (i, m) in model["merges"].as_array().ok_or_else(bad)?.iter().enumerate() {
            // Older files store "a b", newer ones ["a", "b"].
            let pair = match m {
                serde_json::Value::String(s) => s.split_once(' ').map(|(a, b)| (a.to_owned(), b.to_owned())),
                serde_json::Value::Array(a) => match (a.first().and_then(|v| v.as_str()), a.get(1).and_then(|v| v.as_str())) {
                    (Some(a), Some(b)) => Some((a.to_owned(), b.to_owned())),
                    _ => None,
                },
                _ => None,
            };
            if let Some(pair) = pair {
                ranks.entry(pair).or_insert(i);
            }
        }
        if vocab.get("<|startoftext|>") != Some(&START) || vocab.get("<|endoftext|>") != Some(&END) {
            return Err(Error::Model(format!("{}: not a CLIP tokenizer", path.display())));
        }
        Ok(Tokenizer { vocab, ranks, byte_chars: byte_chars() })
    }

    /// Token ids for a text: START, the words (cut to fit), END.
    pub fn encode(&self, text: &str) -> Vec<i64> {
        let mut ids = vec![START];
        let text = text.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
        'words: for word in split_words(&text) {
            let chars: String = word.bytes().map(|b| self.byte_chars[b as usize]).collect();
            for piece in self.bpe(&chars) {
                if ids.len() == MAX_TOKENS - 1 {
                    break 'words;
                }
                // Every byte character is in the vocabulary, so this always finds something.
                if let Some(&id) = self.vocab.get(&piece) {
                    ids.push(id);
                }
            }
        }
        ids.push(END);
        ids
    }

    /// Split a word into vocabulary pieces. The last character carries the `</w>` end marker.
    fn bpe(&self, word: &str) -> Vec<String> {
        let mut parts: Vec<String> = word.chars().map(String::from).collect();
        if let Some(last) = parts.last_mut() {
            last.push_str("</w>");
        }
        loop {
            let best = parts
                .windows(2)
                .enumerate()
                .filter_map(|(i, w)| self.ranks.get(&(w[0].clone(), w[1].clone())).map(|r| (*r, i)))
                .min();
            let Some((_, i)) = best else { break };
            let merged = format!("{}{}", parts[i], parts[i + 1]);
            parts.splice(i..i + 2, [merged]);
        }
        parts
    }
}

/// CLIP's word pattern: contractions, runs of letters, single digits, runs of other symbols.
fn split_words(text: &str) -> Vec<&str> {
    const CONTRACTIONS: &[&str] = &["'s", "'t", "'re", "'ve", "'m", "'ll", "'d"];
    let other = |c: char| !c.is_whitespace() && !c.is_alphabetic() && !c.is_numeric();
    let mut words = Vec::new();
    let mut rest = text;
    while let Some(c) = rest.chars().next() {
        let len = if c.is_whitespace() {
            rest = &rest[c.len_utf8()..];
            continue;
        } else if let Some(k) = CONTRACTIONS.iter().find(|k| rest.starts_with(**k)) {
            k.len()
        } else if c.is_alphabetic() {
            rest.find(|c: char| !c.is_alphabetic()).unwrap_or(rest.len())
        } else if c.is_numeric() {
            c.len_utf8()
        } else {
            rest.find(|c: char| !other(c)).unwrap_or(rest.len())
        };
        words.push(&rest[..len]);
        rest = &rest[len..];
    }
    words
}

/// GPT-2's reversible byte-to-character table: printable bytes map to themselves, the rest
/// to characters from U+0100 up.
fn byte_chars() -> [char; 256] {
    let mut table = ['\0'; 256];
    let mut next = 256u32;
    for b in 0..=255u8 {
        let printable = matches!(b, b'!'..=b'~' | 0xA1..=0xAC | 0xAE..=0xFF);
        table[b as usize] = if printable {
            b as char
        } else {
            let c = char::from_u32(next).unwrap();
            next += 1;
            c
        };
    }
    table
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_like_clip() {
        assert_eq!(split_words("a dog's toy, 2024!!"), ["a", "dog", "'s", "toy", ",", "2", "0", "2", "4", "!!"]);
        assert_eq!(split_words("  café  "), ["café"]);
    }

    #[test]
    fn byte_table_is_reversible() {
        let t = byte_chars();
        let unique: std::collections::HashSet<_> = t.iter().collect();
        assert_eq!(unique.len(), 256);
        assert_eq!(t[b'a' as usize], 'a');
        assert_eq!(t[b' ' as usize], 'Ġ');
    }

    #[test]
    fn encodes_known_ids() {
        let Some(dir) = crate::test_models() else { return };
        let tok = Tokenizer::open(&dir.join(crate::CLIP_TOKENIZER)).unwrap();
        // Ids from OpenAI's CLIP tokenizer.
        assert_eq!(tok.encode("a photo of a dog"), [START, 320, 1125, 539, 320, 1929, END]);
        assert_eq!(tok.encode("A   Photo OF a DOG"), tok.encode("a photo of a dog"));
        // Long text is cut to the model's limit and still ends with END.
        let long = tok.encode(&"sunset ".repeat(200));
        assert_eq!(long.len(), MAX_TOKENS);
        assert_eq!(*long.last().unwrap(), END);
        // Unusual characters still produce tokens.
        assert!(tok.encode("🐶 über").len() > 3);
    }
}
