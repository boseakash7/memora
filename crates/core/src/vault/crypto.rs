//! Cryptography for the locked folder.
//!
//! - A random 256-bit **master key** encrypts everything. It is stored only wrapped (sealed)
//!   twice: by a key derived from the password with Argon2id, and by the recovery key.
//!   Changing the password re-wraps the master key; nothing else is re-encrypted.
//! - Records (an item's name, dates, place, file key...) are sealed with XChaCha20-Poly1305:
//!   `nonce (24 bytes) || ciphertext + tag`, with the record's id as associated data so a
//!   record can't be swapped for another one.
//! - Files are encrypted in 64 KiB chunks so any byte range can be decrypted on its own
//!   (HTTP Range, video seeking). Each chunk's nonce is the file's random prefix plus the chunk
//!   number, and its associated data holds the file header, the chunk number and a "last
//!   chunk" flag: reordered, dropped, truncated or changed chunks fail to decrypt.

use std::io::{self, Read, Seek, SeekFrom, Write};

use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, Zeroizing};

use crate::{Error, Result};

pub const KEY_LEN: usize = 32;
/// A key that is wiped from memory when dropped.
pub type SecretKey = Zeroizing<[u8; KEY_LEN]>;

const NONCE_LEN: usize = 24;
const TAG_LEN: usize = 16;

pub fn random_bytes<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    getrandom::fill(&mut b).expect("the system random number generator failed");
    b
}

pub fn random_key() -> SecretKey {
    Zeroizing::new(random_bytes())
}

/// A random id as 32 hex digits (record ids, file names).
pub fn random_id() -> String {
    hex(&random_bytes::<16>())
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

fn cipher(key: &[u8; KEY_LEN]) -> XChaCha20Poly1305 {
    XChaCha20Poly1305::new(&(*key).into())
}

/// Encrypt and authenticate `plain`; `aad` must be given again to open it.
pub fn seal(key: &[u8; KEY_LEN], aad: &[u8], plain: &[u8]) -> Vec<u8> {
    let nonce: [u8; NONCE_LEN] = random_bytes();
    let ct = cipher(key)
        .encrypt(&XNonce::from(nonce), Payload { msg: plain, aad })
        .expect("XChaCha20-Poly1305 encryption cannot fail for in-memory data");
    let mut out = Vec::with_capacity(NONCE_LEN + ct.len());
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ct);
    out
}

/// Decrypt what [`seal`] made. Fails with the wrong key, wrong `aad`, or changed bytes.
pub fn open(key: &[u8; KEY_LEN], aad: &[u8], sealed: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
    if sealed.len() < NONCE_LEN + TAG_LEN {
        return Err(Error::invalid("encrypted data is damaged"));
    }
    let (nonce, ct) = sealed.split_at(NONCE_LEN);
    let nonce: [u8; NONCE_LEN] = nonce.try_into().expect("split at NONCE_LEN");
    cipher(key)
        .decrypt(&XNonce::from(nonce), Payload { msg: ct, aad })
        .map(Zeroizing::new)
        .map_err(|_| Error::invalid("encrypted data is damaged or the key is wrong"))
}

// ---------------------------------------------------------------------------------------------
// Password and recovery key

/// Argon2id settings, stored with the vault so they can be raised later.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KdfParams {
    /// Memory in KiB.
    pub m_kib: u32,
    pub passes: u32,
    pub lanes: u32,
    /// 16 random bytes, hex.
    pub salt: String,
}

impl Default for KdfParams {
    fn default() -> Self {
        Self::new()
    }
}

impl KdfParams {
    /// 256 MiB, 3 passes, 4 lanes: ~0.1 s on an 8-core desktop (lanes run in parallel), up to
    /// about a second on a slow laptop. Every password guess costs an attacker the same.
    pub fn new() -> Self {
        KdfParams { m_kib: 256 * 1024, passes: 3, lanes: 4, salt: hex(&random_bytes::<16>()) }
    }

    /// Cheap settings so tests run fast. Never used by the app.
    #[cfg(test)]
    pub fn for_tests() -> Self {
        KdfParams { m_kib: 256, passes: 1, lanes: 1, salt: hex(&random_bytes::<16>()) }
    }
}

/// The key that wraps the master key, derived from the password.
pub fn password_key(password: &str, params: &KdfParams) -> Result<SecretKey> {
    let salt = unhex(&params.salt).ok_or_else(|| Error::invalid("bad vault salt"))?;
    let p = Params::new(params.m_kib, params.passes, params.lanes, Some(KEY_LEN))
        .map_err(|e| Error::invalid(format!("bad vault key settings: {e}")))?;
    let mut out = Zeroizing::new([0u8; KEY_LEN]);
    // Normalize nothing: the password is used byte for byte, as typed.
    Argon2::new(Algorithm::Argon2id, Version::V0x13, p)
        .hash_password_into(password.as_bytes(), &salt, out.as_mut())
        .map_err(|e| Error::invalid(format!("cannot derive the vault key: {e}")))?;
    Ok(out)
}

/// Bytes of randomness in a recovery key (160 bits: 32 base32 characters).
pub const RECOVERY_LEN: usize = 20;
/// Crockford's base32: no I, L, O or U, so it is easy to read back and type.
const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// A new recovery key, as shown to the user: 8 groups of 4, like `7KQ2-…`.
pub fn new_recovery_key() -> Zeroizing<String> {
    let bytes = Zeroizing::new(random_bytes::<RECOVERY_LEN>());
    Zeroizing::new(format_recovery(&bytes))
}

fn format_recovery(bytes: &[u8; RECOVERY_LEN]) -> String {
    let mut chars = String::with_capacity(39);
    let mut acc: u32 = 0;
    let mut bits = 0;
    for &b in bytes {
        acc = (acc << 8) | u32::from(b);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            if !chars.is_empty() && (chars.len() + 1).is_multiple_of(5) {
                chars.push('-');
            }
            chars.push(CROCKFORD[((acc >> bits) & 31) as usize] as char);
        }
    }
    acc.zeroize();
    chars
}

/// Read a recovery key back: case, spaces and dashes don't matter, O reads as 0 and I/L as 1.
pub fn parse_recovery_key(s: &str) -> Option<Zeroizing<[u8; RECOVERY_LEN]>> {
    let mut out = Zeroizing::new([0u8; RECOVERY_LEN]);
    let mut acc: u32 = 0;
    let mut bits = 0;
    let mut n = 0;
    for c in s.chars().filter(|c| !c.is_whitespace() && *c != '-') {
        let c = match c.to_ascii_uppercase() {
            'O' => '0',
            'I' | 'L' => '1',
            c => c,
        };
        let v = CROCKFORD.iter().position(|&x| x as char == c)? as u32;
        acc = (acc << 5) | v;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            *out.get_mut(n)? = (acc >> bits) as u8;
            n += 1;
        }
    }
    acc.zeroize();
    (n == RECOVERY_LEN && bits == 0).then_some(out)
}

/// The key that wraps the master key, from a recovery key. The recovery key is fully random,
/// so a plain key derivation is enough (no Argon2).
pub fn recovery_wrap_key(recovery: &[u8; RECOVERY_LEN]) -> SecretKey {
    Zeroizing::new(blake3::derive_key("memora locked folder recovery key v1", recovery))
}

// ---------------------------------------------------------------------------------------------
// Files

const MAGIC: &[u8; 4] = b"MVLT";
const FORMAT: u8 = 1;
/// Plaintext bytes per chunk.
pub const CHUNK: usize = 64 * 1024;
const PREFIX_LEN: usize = 16;
/// magic, format, 3 reserved bytes, chunk size (u32 LE), nonce prefix.
pub const HEADER_LEN: usize = 4 + 1 + 3 + 4 + PREFIX_LEN;
const SEALED_CHUNK: usize = CHUNK + TAG_LEN;

fn chunk_nonce(prefix: &[u8; PREFIX_LEN], index: u64) -> XNonce {
    let mut n = [0u8; NONCE_LEN];
    n[..PREFIX_LEN].copy_from_slice(prefix);
    n[PREFIX_LEN..].copy_from_slice(&index.to_le_bytes());
    XNonce::from(n)
}

fn chunk_aad(header: &[u8; HEADER_LEN], index: u64, last: bool) -> [u8; HEADER_LEN + 9] {
    let mut a = [0u8; HEADER_LEN + 9];
    a[..HEADER_LEN].copy_from_slice(header);
    a[HEADER_LEN..HEADER_LEN + 8].copy_from_slice(&index.to_le_bytes());
    a[HEADER_LEN + 8] = u8::from(last);
    a
}

/// Size of the encrypted file for `plain_len` bytes of plaintext.
pub fn encrypted_len(plain_len: u64) -> u64 {
    let chunks = plain_len.div_ceil(CHUNK as u64).max(1);
    HEADER_LEN as u64 + plain_len + chunks * TAG_LEN as u64
}

/// What an encrypted file holds, from its size: plaintext length and number of chunks.
fn plain_len(file_len: u64) -> Result<(u64, u64)> {
    let damaged = || Error::invalid("encrypted file is damaged");
    let body = file_len.checked_sub(HEADER_LEN as u64).ok_or_else(damaged)?;
    let chunks = body.div_ceil(SEALED_CHUNK as u64).max(1);
    let last = body - (chunks - 1) * SEALED_CHUNK as u64;
    if last < TAG_LEN as u64 {
        return Err(damaged());
    }
    Ok((body - chunks * TAG_LEN as u64, chunks))
}

/// Summary of an encrypted file: plaintext size and BLAKE3 of the plaintext.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Written {
    pub len: u64,
    pub hash: String,
}

/// Encrypt everything `src` gives into `dest`.
pub fn encrypt_stream(key: &[u8; KEY_LEN], mut src: impl Read, mut dest: impl Write) -> Result<Written> {
    let prefix: [u8; PREFIX_LEN] = random_bytes();
    let mut header = [0u8; HEADER_LEN];
    header[..4].copy_from_slice(MAGIC);
    header[4] = FORMAT;
    header[8..12].copy_from_slice(&(CHUNK as u32).to_le_bytes());
    header[12..].copy_from_slice(&prefix);
    dest.write_all(&header)?;

    let cipher = cipher(key);
    let mut hasher = blake3::Hasher::new();
    let mut buf = Zeroizing::new(vec![0u8; CHUNK]);
    let mut next = Zeroizing::new(vec![0u8; CHUNK]);
    let mut filled = read_full(&mut src, &mut buf)?;
    let mut index = 0u64;
    let mut total = 0u64;
    loop {
        // Read ahead one chunk to know whether this one is the last.
        let next_filled = if filled == CHUNK { read_full(&mut src, &mut next)? } else { 0 };
        let last = next_filled == 0;
        let plain = &buf[..filled];
        hasher.update(plain);
        total += filled as u64;
        let ct = cipher
            .encrypt(&chunk_nonce(&prefix, index), Payload { msg: plain, aad: &chunk_aad(&header, index, last) })
            .map_err(|_| Error::invalid("encryption failed"))?;
        dest.write_all(&ct)?;
        if last {
            break;
        }
        std::mem::swap(&mut buf, &mut next);
        filled = next_filled;
        index += 1;
    }
    dest.flush()?;
    Ok(Written { len: total, hash: hasher.finalize().to_hex().to_string() })
}

pub fn encrypt_bytes(key: &[u8; KEY_LEN], data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(encrypted_len(data.len() as u64) as usize);
    encrypt_stream(key, data, &mut out).expect("writing to memory cannot fail");
    out
}

fn read_full(src: &mut impl Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match src.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}

/// Reads the plaintext of an encrypted file, with seeking. Every chunk is checked before any
/// of its bytes are returned; a damaged chunk is an `InvalidData` error.
pub struct Decryptor<R> {
    inner: R,
    cipher: XChaCha20Poly1305,
    header: [u8; HEADER_LEN],
    prefix: [u8; PREFIX_LEN],
    len: u64,
    chunks: u64,
    pos: u64,
    /// The decrypted chunk `cached` (if any).
    buf: Zeroizing<Vec<u8>>,
    cached: Option<u64>,
}

impl<R: Read + Seek> Decryptor<R> {
    pub fn new(key: &[u8; KEY_LEN], mut inner: R) -> Result<Self> {
        let file_len = inner.seek(SeekFrom::End(0))?;
        let (len, chunks) = plain_len(file_len)?;
        inner.seek(SeekFrom::Start(0))?;
        let mut header = [0u8; HEADER_LEN];
        inner.read_exact(&mut header)?;
        if &header[..4] != MAGIC {
            return Err(Error::invalid("not an encrypted Memora file"));
        }
        if header[4] != FORMAT || u32::from_le_bytes(header[8..12].try_into().unwrap()) as usize != CHUNK {
            return Err(Error::invalid("encrypted with a newer version of Memora"));
        }
        let prefix: [u8; PREFIX_LEN] = header[12..].try_into().unwrap();
        Ok(Decryptor {
            inner,
            cipher: cipher(key),
            header,
            prefix,
            len,
            chunks,
            pos: 0,
            buf: Zeroizing::new(Vec::with_capacity(CHUNK)),
            cached: None,
        })
    }

    /// Plaintext size.
    pub fn len(&self) -> u64 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    fn load(&mut self, index: u64) -> io::Result<()> {
        if self.cached == Some(index) {
            return Ok(());
        }
        self.cached = None;
        let last = index + 1 == self.chunks;
        let size = if last {
            (self.len - index * CHUNK as u64) as usize + TAG_LEN
        } else {
            SEALED_CHUNK
        };
        let mut ct = vec![0u8; size];
        self.inner.seek(SeekFrom::Start(HEADER_LEN as u64 + index * SEALED_CHUNK as u64))?;
        self.inner.read_exact(&mut ct)?;
        let plain = self
            .cipher
            .decrypt(
                &chunk_nonce(&self.prefix, index),
                Payload { msg: &ct, aad: &chunk_aad(&self.header, index, last) },
            )
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "encrypted file is damaged or the key is wrong"))?;
        self.buf.zeroize();
        *self.buf = plain;
        self.cached = Some(index);
        Ok(())
    }

    /// Decrypt the whole file and return the BLAKE3 hash of its plaintext (checks every chunk).
    pub fn hash_all(&mut self) -> Result<String> {
        self.seek(SeekFrom::Start(0))?;
        let mut hasher = blake3::Hasher::new();
        let mut buf = Zeroizing::new(vec![0u8; CHUNK]);
        loop {
            let n = self.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        Ok(hasher.finalize().to_hex().to_string())
    }
}

impl<R: Read + Seek> Read for Decryptor<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.pos >= self.len || out.is_empty() {
            // An empty file still has one (empty) chunk; check it so a damaged one is noticed.
            if self.len == 0 && self.cached.is_none() {
                self.load(0)?;
            }
            return Ok(0);
        }
        let index = self.pos / CHUNK as u64;
        self.load(index)?;
        let at = (self.pos - index * CHUNK as u64) as usize;
        let n = out.len().min(self.buf.len() - at);
        out[..n].copy_from_slice(&self.buf[at..at + n]);
        self.pos += n as u64;
        Ok(n)
    }
}

impl<R: Read + Seek> Seek for Decryptor<R> {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let target = match to {
            SeekFrom::Start(n) => Some(n),
            SeekFrom::End(d) => self.len.checked_add_signed(d),
            SeekFrom::Current(d) => self.pos.checked_add_signed(d),
        };
        let target = target.ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "seek before the start"))?;
        self.pos = target;
        Ok(target)
    }
}

/// Decrypt a whole (small) encrypted file held in memory.
pub fn decrypt_bytes(key: &[u8; KEY_LEN], data: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
    let mut d = Decryptor::new(key, io::Cursor::new(data))?;
    let mut out = Zeroizing::new(Vec::with_capacity(d.len() as usize));
    d.read_to_end(&mut out)?;
    Ok(out)
}

#[cfg(test)]
mod tests;
