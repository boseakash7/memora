//! Camera RAW files (CR2, CR3, NEF, ARW, DNG, RAF, ORF, RW2, PEF...).
//!
//! The sensor data is not developed like a RAW editor would. Nearly every camera stores a
//! large JPEG preview in the file (often full size; it is what the camera's own screen
//! shows), and that is the picture we use. Files without one, such as some phone DNGs, get
//! a simple half-size development of their uncompressed sensor data.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use image::RgbImage;

use crate::{Error, Result};

/// What a RAW file offers to show.
#[derive(Debug, Clone, PartialEq)]
pub struct Raw {
    /// Size of the picture, before orientation.
    pub width: u32,
    pub height: u32,
    pub source: Source,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Source {
    /// An embedded JPEG starting at this byte offset.
    Jpeg(u64),
    /// Uncompressed sensor data, see [`develop`].
    Cfa(Box<Cfa>),
}

/// Uncompressed Bayer sensor data and what is needed to turn it into colours.
#[derive(Debug, Clone, PartialEq)]
pub struct Cfa {
    width: u32,
    height: u32,
    bits: u32,
    little_endian: bool,
    strips: Vec<(u64, u64)>,
    /// Colour of each pixel in the repeating 2x2 block, row by row: 0 red, 1 green, 2 blue.
    pattern: [u8; 4],
    black: f32,
    white: f32,
    /// How the camera saw a neutral grey (DNG `AsShotNeutral`), i.e. the white balance.
    neutral: [f32; 3],
    /// XYZ to camera colours (DNG `ColorMatrix`), row-major.
    color_matrix: Option<[f32; 9]>,
}

/// Previews smaller than this on their long side are only used when nothing better exists.
const MIN_PREVIEW: u32 = 1024;

pub fn probe(path: &Path) -> Result<Raw> {
    let mut file = BufReader::new(File::open(path)?);
    let from_tiff = Tiff::open(&mut file).ok().and_then(|mut t| from_tiff(&mut t).ok().flatten());
    let jpeg_ok = |r: &Raw| matches!(r.source, Source::Cfa(_)) || r.width.max(r.height) >= MIN_PREVIEW;
    if let Some(raw) = from_tiff.as_ref().filter(|r| jpeg_ok(r)) {
        return Ok(raw.clone());
    }
    // Not TIFF-based (CR3, RAF...), or no big preview in the IFDs (ORF keeps it in the maker
    // notes): look for embedded JPEGs anywhere in the file.
    let scanned = scan_for_jpegs(&mut file)?;
    let best = [from_tiff, scanned].into_iter().flatten().max_by_key(|r| u64::from(r.width) * u64::from(r.height));
    best.ok_or_else(|| Error::media("this RAW file has no preview picture Memora can show"))
}

// ---------- TIFF structure ----------

#[derive(Debug, Clone, Copy)]
struct Entry {
    typ: u16,
    count: u32,
    /// The value itself when it fits in 4 bytes, else its offset.
    raw: [u8; 4],
}

type Ifd = HashMap<u16, Entry>;

struct Tiff<'a, R> {
    r: &'a mut R,
    le: bool,
}

fn type_size(typ: u16) -> u64 {
    match typ {
        3 | 8 => 2,
        4 | 9 | 11 | 13 => 4,
        5 | 10 | 12 => 8,
        _ => 1,
    }
}

impl<'a, R: Read + Seek> Tiff<'a, R> {
    fn open(r: &'a mut R) -> Result<Self> {
        let mut head = [0u8; 4];
        r.seek(SeekFrom::Start(0))?;
        r.read_exact(&mut head)?;
        // Standard TIFF (42), Olympus ORF ("RO"/"SR") and Panasonic RW2 (0x55) headers.
        let le = match head {
            [b'I', b'I', 42 | 0x52 | 0x53 | 0x55, 0] => true,
            [b'M', b'M', 0, 42] | [b'M', b'M', b'O', b'R'] => false,
            _ => return Err(Error::media("not a TIFF-based file")),
        };
        Ok(Tiff { r, le })
    }

    fn u16(&self, b: [u8; 2]) -> u16 {
        if self.le { u16::from_le_bytes(b) } else { u16::from_be_bytes(b) }
    }

    fn u32(&self, b: [u8; 4]) -> u32 {
        if self.le { u32::from_le_bytes(b) } else { u32::from_be_bytes(b) }
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<()> {
        self.r.seek(SeekFrom::Start(offset))?;
        self.r.read_exact(buf)?;
        Ok(())
    }

    /// One IFD's entries and the offset of the next IFD (0 at the end).
    fn ifd(&mut self, offset: u32) -> Result<(Ifd, u32)> {
        let mut n = [0u8; 2];
        self.read_at(offset.into(), &mut n)?;
        let n = self.u16(n).min(1000) as usize;
        let mut buf = vec![0u8; n * 12 + 4];
        self.read_at(u64::from(offset) + 2, &mut buf)?;
        let mut ifd = Ifd::new();
        for e in buf[..n * 12].chunks_exact(12) {
            let tag = self.u16([e[0], e[1]]);
            let entry = Entry { typ: self.u16([e[2], e[3]]), count: self.u32([e[4], e[5], e[6], e[7]]), raw: [e[8], e[9], e[10], e[11]] };
            ifd.insert(tag, entry);
        }
        let next = self.u32(buf[n * 12..].try_into().unwrap());
        Ok((ifd, next))
    }

    fn bytes(&mut self, e: &Entry, limit: u64) -> Result<Vec<u8>> {
        let len = (type_size(e.typ) * u64::from(e.count)).min(limit);
        if len <= 4 {
            return Ok(e.raw[..len as usize].to_vec());
        }
        let mut buf = vec![0u8; len as usize];
        let offset = self.u32(e.raw);
        self.read_at(offset.into(), &mut buf)?;
        Ok(buf)
    }

    /// Whole-number values (BYTE, SHORT, LONG, IFD), at most `max` of them.
    fn uints(&mut self, e: &Entry, max: u32) -> Result<Vec<u32>> {
        let size = type_size(e.typ);
        if !matches!(e.typ, 1 | 3 | 4 | 13) {
            return Ok(Vec::new());
        }
        let count = e.count.min(max);
        let b = self.bytes(&Entry { count, ..*e }, u64::from(count) * size)?;
        Ok(match size {
            1 => b.iter().map(|&v| u32::from(v)).collect(),
            2 => b.chunks_exact(2).map(|c| u32::from(self.u16([c[0], c[1]]))).collect(),
            _ => b.chunks_exact(4).map(|c| self.u32([c[0], c[1], c[2], c[3]])).collect(),
        })
    }

    fn uint(&mut self, ifd: &Ifd, tag: u16) -> Option<u32> {
        let e = ifd.get(&tag)?;
        self.uints(e, 1).ok()?.first().copied()
    }

    /// Numbers of any numeric type as f32 (rationals divided out).
    fn numbers(&mut self, ifd: &Ifd, tag: u16, max: u32) -> Vec<f32> {
        let Some(e) = ifd.get(&tag).copied() else { return Vec::new() };
        if !matches!(e.typ, 5 | 10) {
            return self.uints(&e, max).unwrap_or_default().into_iter().map(|v| v as f32).collect();
        }
        let count = e.count.min(max);
        let Ok(b) = self.bytes(&Entry { count, ..e }, u64::from(count) * 8) else { return Vec::new() };
        b.chunks_exact(8)
            .map(|c| {
                let (n, d) = (self.u32([c[0], c[1], c[2], c[3]]), self.u32([c[4], c[5], c[6], c[7]]));
                let (n, d) = if e.typ == 10 { (n as i32 as f32, d as i32 as f32) } else { (n as f32, d as f32) };
                if d == 0.0 { 0.0 } else { n / d }
            })
            .collect()
    }

    /// IFD0, the IFDs chained after it and their sub-IFDs (where DNG and NEF keep the raw
    /// data and the full-size preview).
    fn all_ifds(&mut self) -> Result<Vec<Ifd>> {
        let mut first = [0u8; 4];
        self.read_at(4, &mut first)?;
        let mut queue = vec![(self.u32(first), 0)];
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        while let Some((offset, depth)) = queue.pop() {
            if offset == 0 || depth > 3 || out.len() >= 64 || !seen.insert(offset) {
                continue;
            }
            let Ok((ifd, next)) = self.ifd(offset) else { continue };
            queue.push((next, depth));
            if let Some(sub) = ifd.get(&0x14A).copied() {
                for s in self.uints(&sub, 16).unwrap_or_default() {
                    queue.push((s, depth + 1));
                }
            }
            out.push(ifd);
        }
        Ok(out)
    }
}

const CFA: u32 = 32803;
const LINEAR_RAW: u32 = 34892;

fn from_tiff<R: Read + Seek>(t: &mut Tiff<R>) -> Result<Option<Raw>> {
    let ifds = t.all_ifds()?;
    let mut jpegs = Vec::new();
    let mut cfa = None;
    for ifd in &ifds {
        // EXIF-style preview pointer.
        if let (Some(offset), Some(len)) = (t.uint(ifd, 0x201), t.uint(ifd, 0x202))
            && len > 0
        {
            jpegs.push(u64::from(offset));
        }
        // Panasonic RW2 keeps a full JPEG as the value of tag 0x2E.
        if let Some(e) = ifd.get(&0x2E).filter(|e| e.count > 4) {
            jpegs.push(t.u32(e.raw).into());
        }
        let compression = t.uint(ifd, 0x103).unwrap_or(1);
        let photometric = t.uint(ifd, 0x106).unwrap_or(0);
        let strips = match (ifd.get(&0x111).copied(), ifd.get(&0x117).copied()) {
            (Some(o), Some(c)) => {
                let (o, c) = (t.uints(&o, 100_000)?, t.uints(&c, 100_000)?);
                o.into_iter().zip(c).map(|(o, c)| (u64::from(o), u64::from(c))).collect()
            }
            _ => Vec::new(),
        };
        let is_raw_data = photometric == CFA || photometric == LINEAR_RAW;
        if matches!(compression, 6 | 7) && !is_raw_data && strips.len() == 1 {
            jpegs.push(strips[0].0);
        }
        if photometric == CFA && compression == 1 && cfa.is_none() {
            cfa = cfa_info(t, ifd, &ifds[0], strips);
        }
    }

    let best_jpeg = jpegs
        .into_iter()
        .filter_map(|offset| jpeg_size(t.r, offset).map(|(w, h)| (offset, w, h)))
        .max_by_key(|&(_, w, h)| u64::from(w) * u64::from(h));
    let jpeg = best_jpeg.map(|(offset, w, h)| Raw { width: w, height: h, source: Source::Jpeg(offset) });
    let cfa = cfa.map(|c| Raw { width: c.width, height: c.height, source: Source::Cfa(Box::new(c)) });
    Ok(match (jpeg, cfa) {
        (Some(j), _) if j.width.max(j.height) >= MIN_PREVIEW => Some(j),
        (_, Some(c)) => Some(c),
        (j, None) => j,
    })
}

fn cfa_info<R: Read + Seek>(t: &mut Tiff<R>, ifd: &Ifd, ifd0: &Ifd, strips: Vec<(u64, u64)>) -> Option<Cfa> {
    let (width, height) = (t.uint(ifd, 0x100)?, t.uint(ifd, 0x101)?);
    let bits = t.uint(ifd, 0x102).unwrap_or(16);
    let samples = t.uint(ifd, 0x115).unwrap_or(1);
    // Packed 10/12/14-bit data is not supported, only 8 and 16 bits per sample.
    let expected = u64::from(width) * u64::from(height) * u64::from(bits / 8);
    let stored: u64 = strips.iter().map(|s| s.1).sum();
    if samples != 1 || !matches!(bits, 8 | 16) || stored < expected || width < 2 || height < 2 {
        return None;
    }
    let dims = t.numbers(ifd, 0x828D, 2);
    let pattern = t.numbers(ifd, 0x828E, 4);
    if dims != [2.0, 2.0] || pattern.len() != 4 || pattern.iter().any(|&c| c > 2.0) {
        return None;
    }
    let pattern = [pattern[0] as u8, pattern[1] as u8, pattern[2] as u8, pattern[3] as u8];
    let black = t.numbers(ifd, 0xC61A, 16);
    let black = if black.is_empty() { 0.0 } else { black.iter().sum::<f32>() / black.len() as f32 };
    let white = t.numbers(ifd, 0xC61D, 1).first().copied().unwrap_or(((1u32 << bits) - 1) as f32);
    let neutral = t.numbers(ifd0, 0xC628, 3);
    let neutral = if neutral.len() == 3 && neutral.iter().all(|v| *v > 0.0) { [neutral[0], neutral[1], neutral[2]] } else { [1.0; 3] };
    // Prefer the matrix for daylight (illuminant 21, D65), usually ColorMatrix2.
    let matrix = |t: &mut Tiff<R>, tag| <[f32; 9]>::try_from(t.numbers(ifd0, tag, 9)).ok();
    let d65 = |t: &mut Tiff<R>, tag| t.uint(ifd0, tag) == Some(21);
    let color_matrix = if d65(t, 0xC65B) { matrix(t, 0xC622).or_else(|| matrix(t, 0xC621)) } else { None }
        .or_else(|| if d65(t, 0xC65A) { matrix(t, 0xC621) } else { None })
        .or_else(|| matrix(t, 0xC622))
        .or_else(|| matrix(t, 0xC621));
    Some(Cfa {
        width,
        height,
        bits,
        little_endian: t.le,
        strips,
        pattern,
        black,
        white: white.max(black + 1.0),
        neutral,
        color_matrix,
    })
}

// ---------- embedded JPEGs ----------

/// Size of a JPEG that starts at `offset`, if it is one we can decode (8-bit baseline,
/// extended or progressive; not the lossless kind DNGs use for sensor data).
fn jpeg_size<R: Read + Seek>(r: &mut R, offset: u64) -> Option<(u32, u32)> {
    r.seek(SeekFrom::Start(offset)).ok()?;
    let mut b2 = [0u8; 2];
    r.read_exact(&mut b2).ok()?;
    if b2 != [0xFF, 0xD8] {
        return None;
    }
    for _ in 0..64 {
        r.read_exact(&mut b2).ok()?;
        if b2[0] != 0xFF {
            return None;
        }
        let mut marker = b2[1];
        while marker == 0xFF {
            let mut b = [0u8; 1];
            r.read_exact(&mut b).ok()?;
            marker = b[0];
        }
        r.read_exact(&mut b2).ok()?;
        let len = u16::from_be_bytes(b2);
        if len < 2 {
            return None;
        }
        match marker {
            0xC0..=0xC2 => {
                let mut sof = [0u8; 5];
                r.read_exact(&mut sof).ok()?;
                let (h, w) = (u16::from_be_bytes([sof[1], sof[2]]), u16::from_be_bytes([sof[3], sof[4]]));
                return (sof[0] == 8 && w > 0 && h > 0).then_some((w.into(), h.into()));
            }
            0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF | 0xDA | 0xD9 => return None,
            _ => {
                r.seek(SeekFrom::Current(i64::from(len) - 2)).ok()?;
            }
        }
    }
    None
}

/// The biggest decodable JPEG embedded anywhere in the file.
fn scan_for_jpegs<R: Read + Seek>(r: &mut R) -> Result<Option<Raw>> {
    const CHUNK: usize = 1 << 20;
    const MAX_SCAN: u64 = 512 << 20;
    let mut starts = Vec::new();
    let mut buf = vec![0u8; CHUNK + 2];
    let mut pos = 0u64;
    let mut carry = 0usize;
    r.seek(SeekFrom::Start(0))?;
    while pos < MAX_SCAN {
        let n = r.read(&mut buf[carry..])?;
        if n == 0 {
            break;
        }
        let filled = carry + n;
        let base = pos - carry as u64;
        let mut i = 0;
        while i + 2 < filled {
            if buf[i] == 0xFF && buf[i + 1] == 0xD8 && buf[i + 2] == 0xFF {
                starts.push(base + i as u64);
            }
            i += 1;
        }
        // Keep the last two bytes so a marker split across reads is still found.
        let keep = filled.min(2);
        buf.copy_within(filled - keep..filled, 0);
        carry = keep;
        pos += n as u64;
        if starts.len() > 10_000 {
            break;
        }
    }
    let best = starts
        .into_iter()
        .filter_map(|offset| jpeg_size(r, offset).map(|(w, h)| (offset, w, h)))
        .max_by_key(|&(_, w, h)| u64::from(w) * u64::from(h));
    Ok(best.map(|(offset, w, h)| Raw { width: w, height: h, source: Source::Jpeg(offset) }))
}

/// Canon CR3 files keep their EXIF as separate TIFF blocks (`CMT1` camera and orientation,
/// `CMT2` capture settings and date, `CMT4` GPS) instead of one EXIF block.
pub fn cr3_tiff_blocks(path: &Path) -> Vec<Vec<u8>> {
    let mut head = vec![0u8; 256 << 10];
    let Ok(n) = File::open(path).and_then(|mut f| f.read(&mut head)) else { return Vec::new() };
    let head = &head[..n];
    if head.len() < 12 || &head[4..12] != b"ftypcrx " {
        return Vec::new();
    }
    let mut out = Vec::new();
    for name in [b"CMT1", b"CMT2", b"CMT4"] {
        let Some(p) = head.windows(4).position(|w| w == name).filter(|p| *p >= 4) else { continue };
        let size = u32::from_be_bytes(head[p - 4..p].try_into().unwrap()) as usize;
        let (start, end) = (p + 4, (p - 4).saturating_add(size));
        if size > 8 && end <= head.len() {
            out.push(head[start..end].to_vec());
        }
    }
    out
}

// ---------- developing sensor data ----------

/// sRGB (linear, D65) to XYZ.
const XYZ_FROM_SRGB: [f32; 9] = [0.412453, 0.357580, 0.180423, 0.212671, 0.715160, 0.072169, 0.019334, 0.119193, 0.950227];

fn mat_mul(a: &[f32; 9], b: &[f32; 9]) -> [f32; 9] {
    let mut m = [0.0; 9];
    for r in 0..3 {
        for c in 0..3 {
            m[r * 3 + c] = (0..3).map(|k| a[r * 3 + k] * b[k * 3 + c]).sum();
        }
    }
    m
}

fn mat_inverse(m: &[f32; 9]) -> Option<[f32; 9]> {
    let [a, b, c, d, e, f, g, h, i] = *m;
    let det = a * (e * i - f * h) - b * (d * i - f * g) + c * (d * h - e * g);
    if det.abs() < 1e-9 {
        return None;
    }
    let inv = [e * i - f * h, c * h - b * i, b * f - c * e, f * g - d * i, a * i - c * g, c * d - a * f, d * h - e * g, b * g - a * h, a * e - b * d];
    Some(inv.map(|v| v / det))
}

/// Camera colours (white balanced) to linear sRGB, the way dcraw does it: rows are
/// normalised so that a neutral grey stays neutral.
fn srgb_from_camera(color_matrix: &[f32; 9]) -> Option<[f32; 9]> {
    let mut cam_from_srgb = mat_mul(color_matrix, &XYZ_FROM_SRGB);
    for row in cam_from_srgb.chunks_exact_mut(3) {
        let sum: f32 = row.iter().sum();
        if sum.abs() < 1e-6 {
            return None;
        }
        row.iter_mut().for_each(|v| *v /= sum);
    }
    mat_inverse(&cam_from_srgb)
}

fn srgb_gamma(v: f32) -> f32 {
    if v <= 0.003_130_8 { v * 12.92 } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 }
}

/// A half-size picture from uncompressed sensor data: each 2x2 block of the colour filter
/// becomes one pixel. White balance and colour come from the file; brightness is automatic.
pub fn develop(path: &Path, cfa: &Cfa) -> Result<RgbImage> {
    let (w, h) = (cfa.width as usize, cfa.height as usize);
    let bytes_per = (cfa.bits / 8) as usize;
    let mut data = Vec::with_capacity(w * h * bytes_per);
    let mut file = BufReader::new(File::open(path)?);
    for &(offset, len) in &cfa.strips {
        let take = (len as usize).min(w * h * bytes_per - data.len());
        let start = data.len();
        data.resize(start + take, 0);
        file.seek(SeekFrom::Start(offset))?;
        file.read_exact(&mut data[start..])?;
        if data.len() == w * h * bytes_per {
            break;
        }
    }
    let sample = |x: usize, y: usize| -> f32 {
        let i = (y * w + x) * bytes_per;
        match bytes_per {
            1 => f32::from(data[i]),
            _ if cfa.little_endian => f32::from(u16::from_le_bytes([data[i], data[i + 1]])),
            _ => f32::from(u16::from_be_bytes([data[i], data[i + 1]])),
        }
    };

    let (ow, oh) = (w / 2, h / 2);
    let range = cfa.white - cfa.black;
    let matrix = cfa.color_matrix.as_ref().and_then(srgb_from_camera);
    let mut linear = vec![[0f32; 3]; ow * oh];
    for oy in 0..oh {
        for ox in 0..ow {
            let mut sum = [0f32; 3];
            let mut count = [0f32; 3];
            for (k, &color) in cfa.pattern.iter().enumerate() {
                let v = sample(ox * 2 + (k & 1), oy * 2 + (k >> 1));
                sum[color as usize] += ((v - cfa.black) / range).clamp(0.0, 1.0);
                count[color as usize] += 1.0;
            }
            // White balance; clip so blown highlights stay white instead of turning pink.
            let cam: [f32; 3] =
                std::array::from_fn(|c| if count[c] > 0.0 { (sum[c] / count[c] / cfa.neutral[c]).min(1.0) } else { 0.0 });
            linear[oy * ow + ox] = match &matrix {
                Some(m) => std::array::from_fn(|r| (m[r * 3] * cam[0] + m[r * 3 + 1] * cam[1] + m[r * 3 + 2] * cam[2]).max(0.0)),
                None => cam,
            };
        }
    }

    // Brightness: put the 99th percentile of luminance near white (RAW data is usually
    // exposed to keep highlights, so it looks dark as it is).
    let mut lum: Vec<f32> = linear.iter().step_by(7).map(|p| 0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2]).collect();
    let p99 = if lum.is_empty() {
        1.0
    } else {
        let k = (lum.len() - 1) * 99 / 100;
        *lum.select_nth_unstable_by(k, f32::total_cmp).1
    };
    let gain = (0.92 / p99.max(1e-4)).clamp(1.0, 16.0);
    let lut: Vec<u8> = (0..=4096).map(|i| (srgb_gamma(i as f32 / 4096.0) * 255.0).round() as u8).collect();
    let to_u8 = |v: f32| lut[((v * gain).clamp(0.0, 1.0) * 4096.0) as usize];
    Ok(RgbImage::from_fn(ow as u32, oh as u32, |x, y| {
        let p = linear[y as usize * ow + x as usize];
        image::Rgb([to_u8(p[0]), to_u8(p[1]), to_u8(p[2])])
    }))
}

/// Test files: a tiny TIFF writer and RAW files built with it.
#[cfg(test)]
pub(crate) mod test_files {
    use std::io::Cursor;

    /// Minimal little-endian TIFF writer for tests: IFD entries are (tag, type, values).
    pub(crate) fn tiff(ifds: &[Vec<(u16, u16, Vec<u32>)>], blobs: &[&[u8]]) -> Vec<u8> {
        // Layout: header, blobs, then IFDs. Entry values of type 0xFFFF are blob indexes,
        // written as LONG offsets to that blob.
        let mut out = b"II\x2a\x00\0\0\0\0".to_vec();
        let mut blob_at = Vec::new();
        for b in blobs {
            blob_at.push(out.len() as u32);
            out.extend_from_slice(b);
        }
        let first = out.len() as u32;
        out[4..8].copy_from_slice(&first.to_le_bytes());
        for (n, ifd) in ifds.iter().enumerate() {
            let start = out.len();
            let extra_start = start + 2 + ifd.len() * 12 + 4;
            let mut extra = Vec::new();
            out.extend_from_slice(&(ifd.len() as u16).to_le_bytes());
            for (tag, typ, values) in ifd {
                let (typ, values) =
                    if *typ == 0xFFFF { (4, values.iter().map(|&i| blob_at[i as usize]).collect()) } else { (*typ, values.clone()) };
                let mut bytes = Vec::new();
                for v in &values {
                    match typ {
                        3 => bytes.extend_from_slice(&(*v as u16).to_le_bytes()),
                        1 | 2 | 7 => bytes.push(*v as u8),
                        _ => bytes.extend_from_slice(&v.to_le_bytes()),
                    }
                }
                out.extend_from_slice(&tag.to_le_bytes());
                out.extend_from_slice(&typ.to_le_bytes());
                out.extend_from_slice(&(values.len() as u32).to_le_bytes());
                if bytes.len() <= 4 {
                    bytes.resize(4, 0);
                    out.extend_from_slice(&bytes);
                } else {
                    out.extend_from_slice(&((extra_start + extra.len()) as u32).to_le_bytes());
                    extra.extend_from_slice(&bytes);
                }
            }
            let next = if n + 1 < ifds.len() { (extra_start + extra.len()) as u32 } else { 0 };
            out.extend_from_slice(&next.to_le_bytes());
            out.extend_from_slice(&extra);
        }
        out
    }

    pub(crate) fn jpeg(w: u32, h: u32) -> Vec<u8> {
        let mut out = Vec::new();
        image::RgbImage::from_pixel(w, h, image::Rgb([200, 100, 50]))
            .write_to(&mut Cursor::new(&mut out), image::ImageFormat::Jpeg)
            .unwrap();
        out
    }

    /// A camera-style RAW file (like a Canon CR2): a `w` x `h` JPEG preview stored as a strip
    /// in IFD0, with EXIF orientation 6 (turned 90 degrees clockwise) and the make "Canon".
    pub(crate) fn write_raw(path: &std::path::Path, w: u32, h: u32) {
        let preview = jpeg(w, h);
        let make = b"Canon\0";
        let file = tiff(
            &[vec![
                (0x10F, 2, make.iter().map(|&b| u32::from(b)).collect()),
                (0x103, 3, vec![6]),
                (0x106, 3, vec![6]),
                (0x111, 0xFFFF, vec![0]),
                (0x112, 3, vec![6]),
                (0x117, 4, vec![preview.len() as u32]),
            ]],
            &[&preview],
        );
        std::fs::write(path, file).unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::test_files::{jpeg, tiff};
    use super::*;
    use std::io::Cursor;

    #[test]
    fn picks_the_largest_preview_in_the_ifds() {
        let (small, big) = (jpeg(160, 120), jpeg(1200, 800));
        let file = tiff(
            &[
                // IFD0: a thumbnail via the EXIF-style pointer...
                vec![(0x201, 0xFFFF, vec![0]), (0x202, 4, vec![small.len() as u32])],
                // ...and a big preview as a single JPEG strip.
                vec![(0x103, 3, vec![6]), (0x106, 3, vec![6]), (0x111, 0xFFFF, vec![1]), (0x117, 4, vec![big.len() as u32])],
            ],
            &[&small, &big],
        );
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.cr2");
        std::fs::write(&path, &file).unwrap();
        let raw = probe(&path).unwrap();
        assert_eq!((raw.width, raw.height), (1200, 800));
        let Source::Jpeg(offset) = raw.source else { panic!("expected a JPEG preview") };
        assert_eq!(&file[offset as usize..offset as usize + big.len()], &big[..]);
    }

    #[test]
    fn finds_jpegs_in_files_that_are_not_tiff() {
        // Like a Fuji RAF or Canon CR3: a custom header, with JPEGs somewhere inside.
        let mut file = b"FUJIFILMCCD-RAW 0201".to_vec();
        file.extend(std::iter::repeat_n(0xFF, 3000)); // fill bytes must not confuse the scan
        file.extend_from_slice(&jpeg(320, 240));
        file.extend_from_slice(&[0u8; 5000]);
        file.extend_from_slice(&jpeg(1600, 1200));
        let raw = scan_for_jpegs(&mut Cursor::new(&file)).unwrap().unwrap();
        assert_eq!((raw.width, raw.height), (1600, 1200));
    }

    #[test]
    fn lossless_jpeg_sensor_data_is_not_a_preview() {
        let mut lossless = vec![0xFF, 0xD8, 0xFF, 0xC3, 0, 11, 16, 0, 8, 0, 8, 1, 0, 0x11, 0];
        lossless.extend_from_slice(&[0xFF, 0xD9]);
        assert_eq!(jpeg_size(&mut Cursor::new(&lossless), 0), None);
        assert_eq!(jpeg_size(&mut Cursor::new(jpeg(64, 48)), 0), Some((64, 48)));
    }

    #[test]
    fn develops_uncompressed_sensor_data() {
        // 4x4 RGGB sensor: left half lit grey, right half dark; camera sees grey as (0.5, 1, 0.5).
        let (w, h) = (4u32, 4u32);
        let mut pixels = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let green = (x + y) % 2 == 1;
                let lit = x < 2;
                let v: u16 = match (lit, green) {
                    (true, true) => 4000,
                    (true, false) => 2000,
                    (false, _) => 100,
                };
                pixels.extend_from_slice(&v.to_le_bytes());
            }
        }
        let file = tiff(
            &[vec![
                (0x100, 4, vec![w]),
                (0x101, 4, vec![h]),
                (0x102, 3, vec![16]),
                (0x103, 3, vec![1]),
                (0x106, 3, vec![CFA]),
                (0x111, 0xFFFF, vec![0]),
                (0x117, 4, vec![pixels.len() as u32]),
                (0x828D, 3, vec![2, 2]),
                (0x828E, 1, vec![0, 1, 1, 2]),
                (0xC61A, 3, vec![100]),
                (0xC61D, 3, vec![4100]),
            ]],
            &[&pixels],
        );
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.dng");
        std::fs::write(&path, &file).unwrap();
        let raw = probe(&path).unwrap();
        assert_eq!((raw.width, raw.height), (4, 4));
        let Source::Cfa(cfa) = &raw.source else { panic!("expected sensor data") };
        let mut cfa = cfa.clone();
        cfa.neutral = [0.5, 1.0, 0.5];
        let img = develop(&path, &cfa).unwrap();
        assert_eq!(img.dimensions(), (2, 2));
        let lit = img.get_pixel(0, 0).0;
        assert!(lit.iter().all(|&c| c > 200), "white balanced grey comes out bright and neutral: {lit:?}");
        assert!(img.get_pixel(1, 0).0.iter().all(|&c| c < 20), "dark stays dark");
    }

    #[test]
    fn colour_matrix_keeps_grey_neutral() {
        // A typical camera matrix (XYZ to camera).
        let m = srgb_from_camera(&[0.6722, -0.0635, -0.0963, -0.4287, 1.2460, 0.2028, -0.0908, 0.2162, 0.5668]).unwrap();
        for row in m.chunks_exact(3) {
            assert!((row.iter().sum::<f32>() - 1.0).abs() < 1e-4);
        }
    }
}
