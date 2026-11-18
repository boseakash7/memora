use std::io::{Cursor, Read, Seek, SeekFrom};

use super::*;

fn data(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i * 31 + i / 7) as u8).collect()
}

fn decrypt(key: &[u8; KEY_LEN], enc: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut d = Decryptor::new(key, Cursor::new(enc)).map_err(std::io::Error::other)?;
    let mut out = Vec::new();
    d.read_to_end(&mut out)?;
    Ok(out)
}

#[test]
fn sealed_records_need_the_same_key_and_id() {
    let key = random_key();
    let sealed = seal(&key, b"item-1", b"secret name.jpg");
    assert_eq!(open(&key, b"item-1", &sealed).unwrap().as_slice(), b"secret name.jpg");
    assert!(open(&key, b"item-2", &sealed).is_err(), "swapped record");
    assert!(open(&random_key(), b"item-1", &sealed).is_err(), "wrong key");
    let mut bad = sealed.clone();
    *bad.last_mut().unwrap() ^= 1;
    assert!(open(&key, b"item-1", &bad).is_err(), "changed byte");
    assert!(open(&key, b"item-1", &sealed[..10]).is_err(), "too short");
    // Same input, different nonce each time.
    assert_ne!(seal(&key, b"a", b"x"), seal(&key, b"a", b"x"));
}

#[test]
fn recovery_keys_read_back() {
    for _ in 0..50 {
        let shown = new_recovery_key();
        assert_eq!(shown.len(), 39, "{}", *shown);
        assert_eq!(shown.split('-').count(), 8);
        let bytes = parse_recovery_key(&shown).unwrap();
        // Lowercase, spaces instead of dashes, O for 0 and l for 1 all read the same.
        let typed = shown.to_lowercase().replace('-', " ").replace('0', "o").replace('1', "l");
        assert_eq!(*parse_recovery_key(&typed).unwrap(), *bytes);
        assert_eq!(format_recovery(&bytes), *shown);
    }
    assert!(parse_recovery_key("").is_none());
    assert!(parse_recovery_key("ABCD-EFGH").is_none(), "too short");
    assert!(parse_recovery_key(&format!("{}-0000", *new_recovery_key())).is_none(), "too long");
    assert!(parse_recovery_key("UUUU-UUUU-UUUU-UUUU-UUUU-UUUU-UUUU-UUUU").is_none(), "U is not used");
}

#[test]
fn password_keys() {
    let params = KdfParams::for_tests();
    let a = password_key("correct horse", &params).unwrap();
    assert_eq!(*a, *password_key("correct horse", &params).unwrap());
    assert_ne!(*a, *password_key("correct horsE", &params).unwrap());
    let other_salt = KdfParams::for_tests();
    assert_ne!(*a, *password_key("correct horse", &other_salt).unwrap());
    // The real settings are valid (and take a moment).
    let real = KdfParams::new();
    let t = std::time::Instant::now();
    password_key("x", &real).unwrap();
    eprintln!("Argon2id {} MiB/{} passes: {:?}", real.m_kib / 1024, real.passes, t.elapsed());
}

#[test]
fn files_round_trip_at_chunk_edges() {
    let key = random_key();
    for n in [0, 1, CHUNK - 1, CHUNK, CHUNK + 1, 2 * CHUNK, 3 * CHUNK + 5] {
        let plain = data(n);
        let mut enc = Vec::new();
        let w = encrypt_stream(&key, plain.as_slice(), &mut enc).unwrap();
        assert_eq!(w.len, n as u64);
        assert_eq!(w.hash, blake3::hash(&plain).to_hex().to_string());
        assert_eq!(enc.len() as u64, encrypted_len(n as u64), "size for {n}");
        assert_eq!(decrypt(&key, &enc).unwrap(), plain, "round trip {n}");
        let mut d = Decryptor::new(&key, Cursor::new(&enc)).unwrap();
        assert_eq!(d.len(), n as u64);
        assert_eq!(d.hash_all().unwrap(), w.hash);
        assert_eq!(decrypt_bytes(&key, &enc).unwrap().as_slice(), plain.as_slice());
    }
}

#[test]
fn seeking_reads_any_range() {
    let key = random_key();
    let plain = data(5 * CHUNK + 123);
    let enc = encrypt_bytes(&key, &plain);
    let mut d = Decryptor::new(&key, Cursor::new(&enc)).unwrap();
    for (start, len) in [(0, 10), (CHUNK as u64 - 3, 6), (3 * CHUNK as u64, CHUNK + 50), (plain.len() as u64 - 5, 100)] {
        d.seek(SeekFrom::Start(start)).unwrap();
        let mut got = Vec::new();
        (&mut d).take(len as u64).read_to_end(&mut got).unwrap();
        let end = (start as usize + len).min(plain.len());
        assert_eq!(got, plain[start as usize..end], "range {start}+{len}");
    }
    assert_eq!(d.seek(SeekFrom::End(-1)).unwrap(), plain.len() as u64 - 1);
    let mut one = [0u8; 4];
    assert_eq!(d.read(&mut one).unwrap(), 1);
    assert_eq!(one[0], *plain.last().unwrap());
    assert_eq!(d.read(&mut one).unwrap(), 0, "at the end");
    assert!(d.seek(SeekFrom::Current(-(plain.len() as i64) - 5)).is_err());
}

#[test]
fn damaged_files_are_refused() {
    let key = random_key();
    let plain = data(3 * CHUNK + 10);
    let enc = encrypt_bytes(&key, &plain);
    let sealed = SEALED_CHUNK;

    // Wrong key.
    assert!(decrypt(&random_key(), &enc).is_err());

    // A changed byte in the second chunk: the first still reads, the second doesn't.
    let mut bad = enc.clone();
    bad[HEADER_LEN + sealed + 100] ^= 0x40;
    let mut d = Decryptor::new(&key, Cursor::new(&bad)).unwrap();
    let mut first = vec![0u8; CHUNK];
    d.read_exact(&mut first).unwrap();
    assert_eq!(first, plain[..CHUNK]);
    assert!(d.read(&mut first).is_err());

    // A changed header (the nonce prefix is part of every chunk's check).
    let mut bad = enc.clone();
    bad[HEADER_LEN - 1] ^= 1;
    assert!(decrypt(&key, &bad).is_err());

    // Cut at a chunk boundary: the new last chunk was not written as the last one.
    let cut = &enc[..HEADER_LEN + 2 * sealed];
    assert!(decrypt(&key, cut).is_err());
    // Cut inside a chunk.
    assert!(decrypt(&key, &enc[..enc.len() - 3]).is_err());
    // Cut into the header or the tag.
    assert!(Decryptor::new(&key, Cursor::new(&enc[..HEADER_LEN - 1])).is_err());
    assert!(Decryptor::new(&key, Cursor::new(&enc[..HEADER_LEN + sealed + 5])).is_err());

    // Two chunks swapped.
    let mut swapped = enc.clone();
    let (a, b) = (HEADER_LEN, HEADER_LEN + sealed);
    let first: Vec<u8> = swapped[a..a + sealed].to_vec();
    let second: Vec<u8> = swapped[b..b + sealed].to_vec();
    swapped[a..a + sealed].copy_from_slice(&second);
    swapped[b..b + sealed].copy_from_slice(&first);
    assert!(decrypt(&key, &swapped).is_err());

    // A chunk taken from another file with the same key.
    let other = encrypt_bytes(&key, &plain);
    let mut mixed = enc.clone();
    mixed[a..a + sealed].copy_from_slice(&other[a..a + sealed]);
    assert!(decrypt(&key, &mixed).is_err());

    // Not our file at all.
    let mut junk = enc.clone();
    junk[0] = b'X';
    assert!(Decryptor::new(&key, Cursor::new(&junk)).is_err());

    // An empty file with a damaged tag.
    let mut empty = encrypt_bytes(&key, &[]);
    *empty.last_mut().unwrap() ^= 1;
    assert!(decrypt(&key, &empty).is_err());
}
