//! Reads the size, rotation and tile layout of HEIC/HEIF/AVIF photos from the file's boxes,
//! without starting FFmpeg. Decoding still uses FFmpeg (see `ffmpeg::decode_still`).
//!
//! Opening a tiled phone photo in FFmpeg 7.1 starts a decoder thread per tile (48 for a
//! 12 MP photo) and costs ~100 ms and ~200 MB, more than decoding it. Reading the boxes
//! takes well under a millisecond, so indexing opens each photo in FFmpeg only once.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// What the indexer needs to know about a HEIF image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Info {
    /// Size of the primary image as stored, before rotation.
    pub width: u32,
    pub height: u32,
    /// EXIF-style orientation (1-8) from the `irot`/`imir` boxes; None when the file has
    /// neither, in which case EXIF orientation applies.
    pub orientation: Option<u8>,
    /// How FFmpeg 7.1 presents the primary image, when that is certain; else None and FFmpeg
    /// is asked (`ffmpeg::probe_still`).
    pub layout: Option<Layout>,
}

/// The primary image in terms of FFmpeg's streams.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Layout {
    /// One coded image.
    Single { stream: usize },
    /// A grid of tiles: (stream, x, y) in pixels, cropped to the image size from the top left.
    Grid { tiles: Vec<(usize, u32, u32)> },
}

pub fn read(path: &Path) -> Option<Info> {
    // Only `ftyp` and `meta` are read (a few KB even for tiled photos). Some phones write
    // the picture data (`mdat`) first, so the other top-level boxes are skipped over.
    let mut file = File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let mut wanted = Vec::new();
    let mut tracks = false;
    let mut pos = 0u64;
    for _ in 0..32 {
        let mut header = [0u8; 16];
        file.seek(SeekFrom::Start(pos)).ok()?;
        let n = file.read(&mut header).ok()?;
        if n < 8 {
            break;
        }
        let kind: [u8; 4] = header[4..8].try_into().unwrap();
        let size = match u32::from_be_bytes(header[..4].try_into().unwrap()) {
            0 => len - pos,
            1 if n == 16 => u64::from_be_bytes(header[8..16].try_into().unwrap()),
            s => u64::from(s),
        };
        if size < 8 || pos + size > len {
            break;
        }
        tracks |= &kind == b"moov";
        if (&kind == b"ftyp" || &kind == b"meta") && size <= 16 << 20 {
            let mut whole = vec![0u8; size as usize];
            file.seek(SeekFrom::Start(pos)).ok()?;
            file.read_exact(&mut whole).ok()?;
            wanted.extend_from_slice(&whole);
        }
        pos += size;
    }
    let mut read_at = |offset: u64, len: usize| -> Option<Vec<u8>> {
        let mut buf = vec![0u8; len];
        file.seek(SeekFrom::Start(offset)).ok()?;
        file.read_exact(&mut buf).ok()?;
        Some(buf)
    };
    let mut info = parse(&wanted, &mut read_at)?;
    if tracks {
        // Image sequences and live photos also have tracks, which FFmpeg numbers too.
        info.layout = None;
    }
    Some(info)
}

/// Boxes as (type, body) in `data`.
fn boxes(mut data: &[u8]) -> impl Iterator<Item = ([u8; 4], &[u8])> {
    std::iter::from_fn(move || {
        if data.len() < 8 {
            return None;
        }
        let size = u32::from_be_bytes(data[..4].try_into().unwrap()) as u64;
        let kind: [u8; 4] = data[4..8].try_into().unwrap();
        let (header, size) = match size {
            0 => (8, data.len() as u64),
            1 if data.len() >= 16 => (16, u64::from_be_bytes(data[8..16].try_into().unwrap())),
            _ => (8, size),
        };
        if size < header || size > data.len() as u64 {
            return None;
        }
        let body = &data[header as usize..size as usize];
        data = &data[size as usize..];
        Some((kind, body))
    })
}

fn find<'a>(data: &'a [u8], kind: &[u8; 4]) -> Option<&'a [u8]> {
    boxes(data).find(|(k, _)| k == kind).map(|(_, b)| b)
}

/// Big-endian reader over a box body.
struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(data: &'a [u8]) -> Self {
        Cursor { data, pos: 0 }
    }

    fn bytes(&mut self, n: usize) -> Option<&'a [u8]> {
        let out = self.data.get(self.pos..self.pos.checked_add(n)?)?;
        self.pos += n;
        Some(out)
    }

    /// An unsigned number of `n` bytes (0, 1, 2, 4 or 8).
    fn uint(&mut self, n: usize) -> Option<u64> {
        Some(self.bytes(n)?.iter().fold(0u64, |acc, &b| (acc << 8) | u64::from(b)))
    }

    fn u8(&mut self) -> Option<u8> {
        self.uint(1).map(|v| v as u8)
    }

    fn u16(&mut self) -> Option<u32> {
        self.uint(2).map(|v| v as u32)
    }

    fn u32(&mut self) -> Option<u32> {
        self.uint(4).map(|v| v as u32)
    }

    fn rest(&self) -> &'a [u8] {
        &self.data[self.pos.min(self.data.len())..]
    }
}

/// Where an item's data is: (construction method, offset, length) of its first extent.
type Location = (u16, u64, u64);

fn parse(file: &[u8], read_at: &mut dyn FnMut(u64, usize) -> Option<Vec<u8>>) -> Option<Info> {
    let ftyp = find(file, b"ftyp")?;
    let brands: Vec<&[u8]> = std::iter::once(ftyp.get(..4)?).chain(ftyp.get(8..)?.chunks_exact(4)).collect();
    let known = [&b"heic"[..], b"heix", b"heim", b"heis", b"hevc", b"mif1", b"msf1", b"avif", b"avis"];
    if !brands.iter().any(|b| known.contains(b)) {
        return None;
    }
    // `meta` is a full box: 4 bytes of version and flags before its children.
    let meta = find(file, b"meta")?.get(4..)?;
    let pitm = find(meta, b"pitm")?;
    let primary = if pitm.first()? == &0 { Cursor::new(&pitm[4..]).u16()? } else { Cursor::new(&pitm[4..]).u32()? };

    let iprp = find(meta, b"iprp")?;
    let properties: Vec<([u8; 4], &[u8])> = boxes(find(iprp, b"ipco")?).collect();
    let item_properties = associations(find(iprp, b"ipma")?)?;
    let props_of = |item: u32| -> Vec<([u8; 4], &[u8])> {
        item_properties.get(&item).map(|ids| ids.iter().filter_map(|&i| properties.get(i).copied()).collect()).unwrap_or_default()
    };
    let size_of = |item: u32| -> Option<(u32, u32)> {
        let (_, ispe) = props_of(item).into_iter().find(|(k, _)| k == b"ispe")?;
        let mut c = Cursor::new(ispe);
        c.bytes(4)?;
        Some((c.u32()?, c.u32()?))
    };

    let (width, height) = size_of(primary)?;
    let mut transform: Option<Transform> = None;
    let mut cropped = false;
    for (kind, body) in props_of(primary) {
        match &kind {
            // Transformations apply in the order they are listed.
            b"irot" => transform = Some(transform.unwrap_or_default().rotate_ccw(body.first()? & 3)),
            b"imir" => transform = Some(transform.unwrap_or_default().mirror(body.first()? & 1)),
            b"clap" => cropped = true,
            _ => {}
        }
    }
    if width == 0 || height == 0 {
        return None;
    }

    // Best effort: anything unexpected just means FFmpeg is asked instead.
    let layout = if cropped {
        None
    } else {
        (|| {
            let items = items(find(meta, b"iinf")?)?;
            let stream_of = |id: u32| items.iter().find(|i| i.id == id)?.stream;
            let primary_item = items.iter().find(|i| i.id == primary)?;
            match &primary_item.kind {
                b"hvc1" | b"av01" => Some(Layout::Single { stream: primary_item.stream? }),
                b"grid" => {
                    let tiles = references(find(meta, b"iref")?, b"dimg", primary)?;
                    let (method, offset, len) = *locations(find(meta, b"iloc")?)?.get(&primary)?;
                    let data = match method {
                        0 => read_at(offset, len.min(64) as usize)?,
                        1 => find(meta, b"idat")?.get(offset as usize..(offset + len) as usize)?.to_vec(),
                        _ => return None,
                    };
                    let mut c = Cursor::new(&data);
                    let (_version, flags) = (c.u8()?, c.u8()?);
                    let (rows, cols) = (u32::from(c.u8()?) + 1, u32::from(c.u8()?) + 1);
                    let field = if flags & 1 == 1 { 4 } else { 2 };
                    let (out_w, out_h) = (c.uint(field)? as u32, c.uint(field)? as u32);
                    if (rows * cols) as usize != tiles.len() || (out_w, out_h) != (width, height) {
                        return None;
                    }
                    // Row by row, each tile placed after the previous one (as FFmpeg does).
                    let mut placed = Vec::with_capacity(tiles.len());
                    let mut y = 0;
                    for row in tiles.chunks(cols as usize) {
                        let mut x = 0;
                        for &tile in row {
                            let (w, _) = size_of(tile)?;
                            placed.push((stream_of(tile)?, x, y));
                            x += w;
                        }
                        y += size_of(row[0])?.1;
                    }
                    (placed.len() == tiles.len()).then_some(Layout::Grid { tiles: placed })
                }
                _ => None,
            }
        })()
    };
    Some(Info { width, height, orientation: transform.map(Transform::exif), layout })
}

/// Item id -> indexes (0-based) of its properties in `ipco`.
fn associations(ipma: &[u8]) -> Option<HashMap<u32, Vec<usize>>> {
    let mut c = Cursor::new(ipma);
    let version = c.u8()?;
    let flags = c.uint(3)?;
    let mut out = HashMap::new();
    for _ in 0..c.u32()? {
        let item = if version < 1 { c.u16()? } else { c.u32()? };
        let count = c.u8()?;
        let mut list = Vec::new();
        for _ in 0..count {
            let index = if flags & 1 == 1 { c.u16()? & 0x7FFF } else { u32::from(c.u8()? & 0x7F) };
            if index > 0 {
                list.push(index as usize - 1);
            }
        }
        out.insert(item, list);
    }
    Some(out)
}

struct Item {
    id: u32,
    kind: [u8; 4],
    /// FFmpeg's stream index: FFmpeg 7.1 makes a stream for every `hvc1` and `av01` item,
    /// in the order they are listed (and ignores `infe` boxes older than version 2).
    stream: Option<usize>,
}

fn items(iinf: &[u8]) -> Option<Vec<Item>> {
    let mut c = Cursor::new(iinf);
    let version = c.u8()?;
    c.bytes(3)?;
    let count = if version == 0 { c.u16()? } else { c.u32()? };
    let mut items = Vec::new();
    let mut streams = 0;
    for (kind, body) in boxes(c.rest()).take(count as usize) {
        if &kind != b"infe" {
            continue;
        }
        let mut e = Cursor::new(body);
        let version = e.u8()?;
        e.bytes(3)?;
        if version < 2 {
            continue;
        }
        let id = if version == 2 { e.u16()? } else { e.u32()? };
        e.u16()?; // protection index
        let kind: [u8; 4] = e.bytes(4)?.try_into().ok()?;
        let stream = matches!(&kind, b"hvc1" | b"av01").then(|| {
            streams += 1;
            streams - 1
        });
        items.push(Item { id, kind, stream });
    }
    Some(items)
}

/// Items that `from` refers to with a reference of type `kind`, in order.
fn references(iref: &[u8], kind: &[u8; 4], from: u32) -> Option<Vec<u32>> {
    let version = *iref.first()?;
    for (k, body) in boxes(iref.get(4..)?) {
        let mut c = Cursor::new(body);
        let id = if version == 0 { c.u16()? } else { c.u32()? };
        if &k != kind || id != from {
            continue;
        }
        let n = c.u16()?;
        return (0..n).map(|_| if version == 0 { c.u16() } else { c.u32() }).collect();
    }
    None
}

/// Item id -> location of its data.
fn locations(iloc: &[u8]) -> Option<HashMap<u32, Location>> {
    let mut c = Cursor::new(iloc);
    let version = c.u8()?;
    c.bytes(3)?;
    let sizes = c.u16()?;
    let (offset_size, length_size, base_size) = ((sizes >> 12) as usize, (sizes >> 8 & 0xF) as usize, (sizes >> 4 & 0xF) as usize);
    let index_size = if version >= 1 { (sizes & 0xF) as usize } else { 0 };
    let count = if version < 2 { c.u16()? } else { c.u32()? };
    let mut out = HashMap::new();
    for _ in 0..count {
        let id = if version < 2 { c.u16()? } else { c.u32()? };
        let method = if version >= 1 { (c.u16()? & 0xF) as u16 } else { 0 };
        c.u16()?; // data reference index
        let base = c.uint(base_size)?;
        let extents = c.u16()?;
        let mut first = None;
        for _ in 0..extents {
            c.uint(index_size)?;
            let (offset, length) = (c.uint(offset_size)?, c.uint(length_size)?);
            first.get_or_insert((method, base + offset, length));
        }
        if let Some(loc) = first {
            out.insert(id, loc);
        }
    }
    Some(out)
}

/// A display transformation: mirror left-right first (if `flip`), then turn clockwise
/// `quarter_turns` times. Every EXIF orientation is one of these eight.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Transform {
    flip: bool,
    quarter_turns: u8,
}

impl Transform {
    /// `irot`: counter-clockwise by `angle` x 90 degrees.
    fn rotate_ccw(self, angle: u8) -> Self {
        Transform { quarter_turns: (self.quarter_turns + 4 - angle) % 4, ..self }
    }

    /// `imir`: axis 0 mirrors left-right, axis 1 top-bottom (= left-right, then half a turn).
    fn mirror(self, axis: u8) -> Self {
        // Mirroring after a turn equals turning the other way after mirroring.
        let flipped = Transform { flip: !self.flip, quarter_turns: (4 - self.quarter_turns) % 4 };
        if axis == 1 { flipped.rotate_ccw(2) } else { flipped }
    }

    fn exif(self) -> u8 {
        match (self.flip, self.quarter_turns) {
            (false, 0) => 1,
            (false, 1) => 6,
            (false, 2) => 3,
            (false, _) => 8,
            (true, 0) => 2,
            (true, 1) => 7,
            (true, 2) => 4,
            (true, _) => 5,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bx(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut out = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        out.extend_from_slice(kind);
        out.extend_from_slice(body);
        out
    }

    fn infe(id: u16, kind: &[u8; 4]) -> Vec<u8> {
        let mut body = vec![2, 0, 0, 0];
        body.extend_from_slice(&id.to_be_bytes());
        body.extend_from_slice(&[0, 0]);
        body.extend_from_slice(kind);
        body.push(0); // empty name
        bx(b"infe", &body)
    }

    /// A HEIC header like an iPhone's: item 49 is the primary image, a 2x2 grid of 512 px
    /// tiles (items 1-4) cropped to 1000x750, stored in `idat`; an Exif item sits between
    /// the tiles in `iinf`, and the size and `irot` are in `ipco`.
    fn heic(irot: Option<u8>, imir: Option<u8>) -> Vec<u8> {
        let mut ipco = bx(b"ispe", &[0, 0, 0, 0, 0, 0, 0x03, 0xE8, 0, 0, 0x02, 0xEE]); // #1: 1000 x 750
        ipco.extend(bx(b"ispe", &[0, 0, 0, 0, 0, 0, 2, 0, 0, 0, 2, 0])); // #2: 512 x 512 tile
        let mut props = vec![0x81u8]; // the grid's size, essential
        let mut idx = 3u8;
        if let Some(a) = irot {
            ipco.extend(bx(b"irot", &[a]));
            props.push(0x80 | idx);
            idx += 1;
        }
        if let Some(a) = imir {
            ipco.extend(bx(b"imir", &[a]));
            props.push(0x80 | idx);
        }
        // ipma v0 flags 0: tiles 1-4 -> #2, item 49 -> props
        let mut ipma = vec![0, 0, 0, 0, 0, 0, 0, 5];
        for tile in 1..=4u8 {
            ipma.extend_from_slice(&[0, tile, 1, 0x82]);
        }
        ipma.extend_from_slice(&[0, 49, props.len() as u8]);
        ipma.extend(&props);
        let iprp = bx(b"iprp", &[bx(b"ipco", &ipco), bx(b"ipma", &ipma)].concat());
        let iinf = bx(
            b"iinf",
            &[vec![0, 0, 0, 0, 0, 6], infe(1, b"hvc1"), infe(2, b"hvc1"), infe(50, b"Exif"), infe(3, b"hvc1"), infe(4, b"hvc1"), infe(49, b"grid")].concat(),
        );
        let iref = bx(b"iref", &[vec![0, 0, 0, 0], bx(b"dimg", &[0, 49, 0, 4, 0, 1, 0, 2, 0, 3, 0, 4])].concat());
        // iloc v1: offset/length 4 bytes, no base; item 49 in idat (method 1) at 0, 8 bytes.
        let iloc = bx(b"iloc", &[1, 0, 0, 0, 0x44, 0x00, 0, 1, 0, 49, 0, 1, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 8]);
        // grid: version 0, flags 0, rows-1 = 1, cols-1 = 1, 1000 x 750.
        let idat = bx(b"idat", &[0, 0, 1, 1, 0x03, 0xE8, 0x02, 0xEE]);
        let meta = bx(b"meta", &[vec![0, 0, 0, 0], bx(b"pitm", &[0, 0, 0, 0, 0, 49]), iinf, iref, iprp, iloc, idat].concat());
        [bx(b"ftyp", b"heic\0\0\0\0mif1heic"), meta].concat()
    }

    fn info(file: &[u8]) -> Option<Info> {
        parse(file, &mut |_, _| None)
    }

    #[test]
    fn reads_size_and_rotation_of_the_primary_image() {
        let plain = info(&heic(None, None)).unwrap();
        assert_eq!((plain.width, plain.height, plain.orientation), (1000, 750, None));
        // iPhone portrait photos: irot 3 (270° counter-clockwise) with EXIF 6, irot 1 with EXIF 8.
        assert_eq!(info(&heic(Some(3), None)).unwrap().orientation, Some(6));
        assert_eq!(info(&heic(Some(1), None)).unwrap().orientation, Some(8));
        assert_eq!(info(&heic(Some(2), None)).unwrap().orientation, Some(3));
        assert_eq!(info(&heic(Some(0), None)).unwrap().orientation, Some(1));
        assert_eq!(info(&heic(None, Some(0))).unwrap().orientation, Some(2));
        assert_eq!(info(&heic(None, Some(1))).unwrap().orientation, Some(4));
        // Turn 90° clockwise, then mirror left-right: EXIF 5 (transpose).
        assert_eq!(info(&heic(Some(3), Some(0))).unwrap().orientation, Some(5));
    }

    #[test]
    fn grid_layout_matches_ffmpeg_streams() {
        let layout = info(&heic(Some(3), None)).unwrap().layout;
        // The Exif item gets no stream, so the tiles are streams 0-3.
        let tiles = vec![(0, 0, 0), (1, 512, 0), (2, 0, 512), (3, 512, 512)];
        assert_eq!(layout, Some(Layout::Grid { tiles }));
    }

    #[test]
    fn other_files_are_not_heif() {
        assert_eq!(info(b"\xFF\xD8\xFF\xE0 not a heic"), None);
        assert_eq!(info(&[bx(b"ftyp", b"isom\0\0\0\0mp41"), bx(b"moov", &[])].concat()), None);
    }
}
