//! The library as a DLNA content tree, and its DIDL-Lite XML.
//!
//! Object ids:
//! - `0` root; `t` Timeline, `t:2024` a year, `t:2024-03` a month
//! - `a` Albums, `a:12` an album; `h` People, `h:7` a person; `p` Places, `p:IN` a country, `pl:45` a place
//! - `f` Favorites, `v` Videos; `d` Folders, `d:3` a watched folder, `d:3:Trip/Day 2` inside it
//! - items: `<parent id>|<media id>` (Windows file names can't contain `|`)

use std::collections::BTreeMap;

use photos_core::db::Db;
use photos_core::media::{self, Kind, Media, Query, Scope};
use photos_core::{Error, Result, albums, browse, folders, people, places, thumbs, transcode};

use super::countries;
use super::soap::escape;
use crate::lan::files::dlna_video_mime;
use crate::stream::npt;

const FLAGS_IMAGE: &str = "00D00000000000000000000000000000";

/// `contentFeatures.dlna.org` for an original video (byte seeking).
pub const VIDEO_ORIGINAL: &str = "DLNA.ORG_OP=01;DLNA.ORG_CI=0;DLNA.ORG_FLAGS=01700000000000000000000000000000";
/// For a converted video: no length, so seeking is by time only.
pub const VIDEO_CONVERTED: &str = "DLNA.ORG_OP=10;DLNA.ORG_CI=1;DLNA.ORG_FLAGS=01700000000000000000000000000000";
pub const THUMB_FEATURES: &str = "DLNA.ORG_PN=JPEG_TN;DLNA.ORG_OP=01;DLNA.ORG_CI=1;DLNA.ORG_FLAGS=00D00000000000000000000000000000";

/// `contentFeatures.dlna.org` for the TV photo of `m`.
pub fn photo_features(m: &Media) -> String {
    format!("DLNA.ORG_PN={};DLNA.ORG_OP=01;DLNA.ORG_CI=1;DLNA.ORG_FLAGS={FLAGS_IMAGE}", photo_profile(m))
}

/// DLNA JPEG profile by size: SM up to 640x480, MED up to 1024x768, else LRG (up to 4096).
fn photo_profile(m: &Media) -> &'static str {
    match thumbs::tv_size(m) {
        Some((w, h)) if w.max(h) <= 640 && w.min(h) <= 480 => "JPEG_SM",
        Some((w, h)) if w.max(h) <= 1024 && w.min(h) <= 768 => "JPEG_MED",
        _ => "JPEG_LRG",
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Obj {
    Root,
    Timeline,
    Year(String),
    Month(String),
    Albums,
    Album(i64),
    People,
    Person(i64),
    Places,
    Country(String),
    Place(i64),
    Favorites,
    Videos,
    Folders,
    Folder(i64, String),
    Item(String, i64),
}

impl Obj {
    fn parse(id: &str) -> Option<Obj> {
        if let Some((parent, item)) = id.rsplit_once('|') {
            return Some(Obj::Item(parent.to_owned(), item.parse().ok()?));
        }
        let (kind, rest) = id.split_once(':').unwrap_or((id, ""));
        let digits = |s: &str| s.parse::<i64>().ok();
        Some(match (kind, rest) {
            ("0", "") => Obj::Root,
            ("t", "") => Obj::Timeline,
            ("t", y) if y.len() == 4 => Obj::Year(y.to_owned()),
            ("t", m) if m.len() == 7 => Obj::Month(m.to_owned()),
            ("a", "") => Obj::Albums,
            ("a", n) => Obj::Album(digits(n)?),
            ("h", "") => Obj::People,
            ("h", n) => Obj::Person(digits(n)?),
            ("p", "") => Obj::Places,
            ("p", cc) => Obj::Country(cc.to_owned()),
            ("pl", n) => Obj::Place(digits(n)?),
            ("f", "") => Obj::Favorites,
            ("v", "") => Obj::Videos,
            ("d", "") => Obj::Folders,
            ("d", rest) => {
                let (n, rel) = rest.split_once(':').unwrap_or((rest, ""));
                Obj::Folder(digits(n)?, rel.to_owned())
            }
            _ => return None,
        })
    }

    fn id(&self) -> String {
        match self {
            Obj::Root => "0".into(),
            Obj::Timeline => "t".into(),
            Obj::Year(y) | Obj::Month(y) => format!("t:{y}"),
            Obj::Albums => "a".into(),
            Obj::Album(n) => format!("a:{n}"),
            Obj::People => "h".into(),
            Obj::Person(n) => format!("h:{n}"),
            Obj::Places => "p".into(),
            Obj::Country(cc) => format!("p:{cc}"),
            Obj::Place(n) => format!("pl:{n}"),
            Obj::Favorites => "f".into(),
            Obj::Videos => "v".into(),
            Obj::Folders => "d".into(),
            Obj::Folder(n, rel) if rel.is_empty() => format!("d:{n}"),
            Obj::Folder(n, rel) => format!("d:{n}:{rel}"),
            Obj::Item(parent, n) => format!("{parent}|{n}"),
        }
    }
}

/// A folder in the tree.
#[derive(Debug, Clone, PartialEq)]
pub struct Container {
    pub id: String,
    pub parent: String,
    pub title: String,
    pub count: u64,
    pub album: bool,
    pub cover: Option<i64>,
}

#[derive(Debug, Clone)]
pub enum Node {
    Container(Container),
    Item { parent: String, media: Box<Media> },
}

const MONTHS: [&str; 12] =
    ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];

fn month_title(ym: &str) -> String {
    let month: usize = ym.get(5..7).and_then(|m| m.parse().ok()).unwrap_or(0);
    match MONTHS.get(month.wrapping_sub(1)) {
        Some(name) => format!("{name} {}", &ym[..4]),
        None => ym.to_owned(),
    }
}

fn container(obj: &Obj, parent: &str, title: impl Into<String>, count: u64, cover: Option<i64>) -> Container {
    Container { id: obj.id(), parent: parent.into(), title: title.into(), count, album: matches!(obj, Obj::Album(_)), cover }
}

fn count(db: &Db, q: &Query) -> Result<u64> {
    Ok(media::page(db, q, 0, 1)?.1)
}

fn month_query(ym: &str) -> Query {
    let year: i32 = ym[..4].parse().unwrap_or(2000);
    let month: u32 = ym[5..].parse().unwrap_or(1);
    let last = match month {
        2 if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    Query { from: Some(format!("{ym}-01")), to: Some(format!("{ym}-{last}")), ..Default::default() }
}

fn videos_query() -> Query {
    Query { kind: Some("video".into()), ..Default::default() }
}

fn favorites_query() -> Query {
    Query { scope: Scope::Favorites, ..Default::default() }
}

/// People shown on TVs: named and not hidden (unnamed groups are often strangers), with
/// the photo their face picture comes from.
fn named_people(db: &Db) -> Result<Vec<(people::Person, Option<i64>)>> {
    people::list(db, false)?
        .into_iter()
        .filter(|p| p.name.is_some())
        .map(|p| {
            let cover = p.cover_face.map(|f| people::media_of_face(db, f).map(|(m, _)| m)).transpose()?;
            Ok((p, cover))
        })
        .collect()
}

fn person_container(p: &people::Person, cover: Option<i64>) -> Container {
    container(&Obj::Person(p.id), "h", p.name.clone().unwrap_or_default(), p.count as u64, cover)
}

/// Places grouped by country code: (code, places).
fn countries(db: &Db) -> Result<BTreeMap<String, Vec<places::Place>>> {
    let mut map: BTreeMap<String, Vec<places::Place>> = BTreeMap::new();
    for p in places::list(db)? {
        map.entry(p.country.clone()).or_default().push(p);
    }
    Ok(map)
}

fn folder_title(db: &Db, id: i64, rel: &str) -> Result<String> {
    match rel.rsplit(['/', '\\']).next().filter(|s| !s.is_empty()) {
        Some(last) => Ok(last.to_owned()),
        None => {
            let f = folders::get(db, id)?;
            Ok(f.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| f.path.display().to_string()))
        }
    }
}

/// The container `obj` describes (title, parent, size), for BrowseMetadata and listings.
fn describe(db: &Db, obj: &Obj, root_title: &str) -> Result<Container> {
    Ok(match obj {
        Obj::Root => container(obj, "-1", root_title, 7, None),
        Obj::Timeline => container(obj, "0", "Timeline", browse::years(db)?.len() as u64, None),
        Obj::Year(y) => {
            let year = browse::years(db)?.into_iter().find(|p| &p.key == y).ok_or(Error::NotFound)?;
            container(obj, "t", y.clone(), browse::months(db, y)?.len() as u64, Some(year.cover))
        }
        Obj::Month(ym) => {
            let m = browse::months(db, &ym[..4])?.into_iter().find(|p| &p.key == ym).ok_or(Error::NotFound)?;
            container(obj, &format!("t:{}", &ym[..4]), month_title(ym), m.count, Some(m.cover))
        }
        Obj::Albums => container(obj, "0", "Albums", albums::list(db)?.iter().filter(|a| a.count > 0).count() as u64, None),
        Obj::Album(n) => {
            let a = albums::get(db, *n)?;
            container(obj, "a", a.name, a.count as u64, a.cover_id)
        }
        Obj::People => container(obj, "0", "People", named_people(db)?.len() as u64, None),
        Obj::Person(n) => {
            let (p, cover) = named_people(db)?.into_iter().find(|(p, _)| p.id == *n).ok_or(Error::NotFound)?;
            person_container(&p, cover)
        }
        Obj::Places => container(obj, "0", "Places", countries(db)?.len() as u64, None),
        Obj::Country(cc) => {
            let list = countries(db)?.remove(cc).ok_or(Error::NotFound)?;
            container(obj, "p", countries::name(cc), list.len() as u64, list[0].cover_id)
        }
        Obj::Place(n) => {
            let p = places::get(db, *n)?;
            let title = if p.city.is_empty() { p.admin1.clone() } else { p.city.clone() };
            container(obj, &format!("p:{}", p.country), title, p.count as u64, p.cover_id)
        }
        Obj::Favorites => container(obj, "0", "Favorites", count(db, &favorites_query())?, None),
        Obj::Videos => container(obj, "0", "Videos", count(db, &videos_query())?, None),
        Obj::Folders => container(obj, "0", "Folders", folders::list(db)?.len() as u64, None),
        Obj::Folder(n, rel) => {
            let listing = browse::folder(db, *n, rel)?;
            let parent = match rel.rsplit_once(['/', '\\']) {
                _ if rel.is_empty() => "d".to_owned(),
                Some((up, _)) => Obj::Folder(*n, up.to_owned()).id(),
                None => Obj::Folder(*n, String::new()).id(),
            };
            let size = (listing.dirs.len() + listing.items.len()) as u64;
            container(obj, &parent, folder_title(db, *n, rel)?, size, listing.items.first().copied())
        }
        Obj::Item(..) => return Err(Error::NotFound),
    })
}

/// One page of a list of containers.
fn page_of<T>(all: Vec<T>, start: u64, count: u64) -> (Vec<T>, u64) {
    let total = all.len() as u64;
    let take = if count == 0 { usize::MAX } else { count as usize };
    (all.into_iter().skip(start as usize).take(take).collect(), total)
}

fn items(db: &Db, parent: &Obj, q: &Query, start: u64, count: u64) -> Result<(Vec<Node>, u64)> {
    let (list, total) = media::page(db, q, start, count)?;
    Ok((wrap_items(db, parent, list), total))
}

fn wrap_items(db: &Db, parent: &Obj, mut list: Vec<Media>) -> Vec<Node> {
    fill_codecs(db, &mut list);
    let parent = parent.id();
    list.into_iter().map(|m| Node::Item { parent: parent.clone(), media: Box::new(m) }).collect()
}

/// Probe videos indexed before codecs were stored, several at a time.
fn fill_codecs(db: &Db, list: &mut [Media]) {
    let todo = list.iter().filter(|m| m.kind == Kind::Video && m.video_codec.is_none()).count();
    if todo == 0 {
        return;
    }
    let chunk = todo.div_ceil(6).max(1);
    let mut videos: Vec<&mut Media> = list.iter_mut().filter(|m| m.kind == Kind::Video && m.video_codec.is_none()).collect();
    std::thread::scope(|s| {
        for part in videos.chunks_mut(chunk) {
            s.spawn(move || {
                for m in part {
                    let _ = media::ensure_codecs(db, m);
                }
            });
        }
    });
}

/// BrowseDirectChildren: one page of `id`'s children and how many there are in all.
pub fn children(db: &Db, id: &str, start: u64, count: u64, root_title: &str) -> Result<(Vec<Node>, u64)> {
    let obj = Obj::parse(id).ok_or(Error::NotFound)?;
    let containers = |list: Vec<Container>| {
        let (page, total) = page_of(list, start, count);
        (page.into_iter().map(Node::Container).collect(), total)
    };
    Ok(match &obj {
        Obj::Root => {
            let tops = [Obj::Timeline, Obj::Albums, Obj::People, Obj::Places, Obj::Favorites, Obj::Videos, Obj::Folders];
            containers(tops.iter().map(|o| describe(db, o, root_title)).collect::<Result<_>>()?)
        }
        Obj::Timeline => containers(
            browse::years(db)?
                .into_iter()
                .map(|y| {
                    let months = browse::months(db, &y.key)?.len() as u64;
                    Ok(container(&Obj::Year(y.key.clone()), "t", y.key, months, Some(y.cover)))
                })
                .collect::<Result<_>>()?,
        ),
        Obj::Year(y) => containers(
            browse::months(db, y)?
                .into_iter()
                .map(|m| container(&Obj::Month(m.key.clone()), &obj.id(), month_title(&m.key), m.count, Some(m.cover)))
                .collect(),
        ),
        Obj::Month(ym) => items(db, &obj, &month_query(ym), start, count)?,
        Obj::Albums => containers(
            albums::list(db)?
                .into_iter()
                .filter(|a| a.count > 0)
                .map(|a| container(&Obj::Album(a.id), "a", a.name, a.count as u64, a.cover_id))
                .collect(),
        ),
        Obj::Album(n) => {
            albums::get(db, *n)?;
            items(db, &obj, &Query { scope: Scope::Album, album: Some(*n), ..Default::default() }, start, count)?
        }
        Obj::People => containers(named_people(db)?.iter().map(|(p, cover)| person_container(p, *cover)).collect()),
        Obj::Person(n) => {
            // Hidden and unnamed people are not on the TV.
            named_people(db)?.into_iter().find(|(p, _)| p.id == *n).ok_or(Error::NotFound)?;
            items(db, &obj, &Query { person: Some(*n), ..Default::default() }, start, count)?
        }
        Obj::Places => containers(
            countries(db)?
                .into_iter()
                .map(|(cc, list)| {
                    container(&Obj::Country(cc.clone()), "p", countries::name(&cc), list.len() as u64, list[0].cover_id)
                })
                .collect(),
        ),
        Obj::Country(cc) => {
            let list = countries(db)?.remove(cc).ok_or(Error::NotFound)?;
            containers(
                list.into_iter()
                    .map(|p| {
                        let title = if p.city.is_empty() { p.admin1 } else { p.city };
                        container(&Obj::Place(p.id), &obj.id(), title, p.count as u64, p.cover_id)
                    })
                    .collect(),
            )
        }
        Obj::Place(n) => {
            places::get(db, *n)?;
            items(db, &obj, &Query { place: Some(*n), ..Default::default() }, start, count)?
        }
        Obj::Favorites => items(db, &obj, &favorites_query(), start, count)?,
        Obj::Videos => items(db, &obj, &videos_query(), start, count)?,
        Obj::Folders => containers(
            folders::list(db)?
                .into_iter()
                .map(|f| describe(db, &Obj::Folder(f.id, String::new()), root_title))
                .collect::<Result<_>>()?,
        ),
        Obj::Folder(n, rel) => {
            let listing = browse::folder(db, *n, rel)?;
            let mut nodes: Vec<Node> = Vec::new();
            let total = (listing.dirs.len() + listing.items.len()) as u64;
            let take = if count == 0 { u64::MAX } else { count };
            let wanted = |i: u64| i >= start && i < start.saturating_add(take);
            for (i, (name, n_items)) in listing.dirs.iter().enumerate() {
                if wanted(i as u64) {
                    let child_rel = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
                    nodes.push(Node::Container(container(&Obj::Folder(*n, child_rel), &obj.id(), name.clone(), *n_items, None)));
                }
            }
            let first_item = listing.dirs.len() as u64;
            let ids: Vec<i64> = listing
                .items
                .iter()
                .enumerate()
                .filter(|(i, _)| wanted(first_item + *i as u64))
                .map(|(_, id)| *id)
                .collect();
            let list: Vec<Media> = ids.iter().filter_map(|id| media::get_visible(db, *id).ok()).collect();
            nodes.extend(wrap_items(db, &obj, list));
            (nodes, total)
        }
        Obj::Item(..) => (Vec::new(), 0),
    })
}

/// BrowseMetadata: the object itself.
pub fn metadata(db: &Db, id: &str, root_title: &str) -> Result<Node> {
    match Obj::parse(id).ok_or(Error::NotFound)? {
        Obj::Item(parent, n) => {
            let mut list = vec![media::get_visible(db, n)?];
            fill_codecs(db, &mut list);
            Ok(Node::Item { parent, media: Box::new(list.remove(0)) })
        }
        obj => Ok(Node::Container(describe(db, &obj, root_title)?)),
    }
}

/// How URLs in the XML are built.
pub struct Urls {
    /// `http://192.168.0.100:8200`
    pub base: String,
    pub transcode: bool,
}

impl Urls {
    fn file(&self, id: i64, file: &str) -> String {
        format!("{}/dlna/m/{id}/{file}", self.base)
    }
}

const DIDL_START: &str = r#"<DIDL-Lite xmlns="urn:schemas-upnp-org:metadata-1-0/DIDL-Lite/" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:upnp="urn:schemas-upnp-org:metadata-1-0/upnp/" xmlns:dlna="urn:schemas-dlna-org:metadata-1-0/">"#;

pub fn didl(nodes: &[Node], urls: &Urls) -> String {
    let mut xml = String::from(DIDL_START);
    for node in nodes {
        match node {
            Node::Container(c) => write_container(&mut xml, c, urls),
            Node::Item { parent, media } => write_item(&mut xml, parent, media, urls),
        }
    }
    xml.push_str("</DIDL-Lite>");
    xml
}

/// Thumbnail URL. The file name is unique per item because some Samsung TVs cache
/// thumbnails by file name alone, which made every item show the same picture.
fn tn_url(urls: &Urls, id: i64) -> String {
    urls.file(id, &format!("tn-{id}.jpg"))
}

fn album_art(xml: &mut String, id: i64, urls: &Urls) {
    xml.push_str(&format!(
        r#"<upnp:albumArtURI dlna:profileID="JPEG_TN">{}</upnp:albumArtURI>"#,
        escape(&tn_url(urls, id))
    ));
}

fn write_container(xml: &mut String, c: &Container, urls: &Urls) {
    let class = if c.album { "object.container.album.photoAlbum" } else { "object.container.storageFolder" };
    xml.push_str(&format!(
        r#"<container id="{}" parentID="{}" restricted="1" searchable="0" childCount="{}"><dc:title>{}</dc:title><upnp:class>{class}</upnp:class>"#,
        escape(&c.id),
        escape(&c.parent),
        c.count,
        escape(&c.title)
    ));
    if let Some(cover) = c.cover {
        album_art(xml, cover, urls);
    }
    xml.push_str("</container>");
}

fn write_item(xml: &mut String, parent: &str, m: &Media, urls: &Urls) {
    let id = Obj::Item(parent.to_owned(), m.id).id();
    let class = if m.kind == Kind::Video { "object.item.videoItem" } else { "object.item.imageItem.photo" };
    xml.push_str(&format!(
        r#"<item id="{}" parentID="{}" restricted="1"><dc:title>{}</dc:title><upnp:class>{class}</upnp:class><dc:date>{}</dc:date>"#,
        escape(&id),
        escape(parent),
        escape(&m.name),
        escape(&m.taken_at)
    ));
    album_art(xml, m.id, urls);

    let res = |info: &str, attrs: &str, url: &str| {
        format!(r#"<res protocolInfo="http-get:*:{info}"{attrs}>{}</res>"#, escape(url))
    };
    // Thumbnail `res` with its pixel size, as servers that Samsung TVs show video pictures
    // for send it.
    let small = thumbs::small_size(m).map(|(w, h)| format!(r#" resolution="{w}x{h}""#)).unwrap_or_default();
    let thumb_res = res(&format!("image/jpeg:{THUMB_FEATURES}"), &small, &tn_url(urls, m.id));
    if m.kind == Kind::Video {
        let duration = m.duration_ms.map(|d| format!(r#" duration="{}""#, npt(d))).unwrap_or_default();
        let resolution = match (m.width, m.height) {
            (Some(w), Some(h)) => format!(r#" resolution="{w}x{h}""#),
            _ => String::new(),
        };
        let ext = m.path.extension().and_then(|e| e.to_str()).unwrap_or("bin").to_ascii_lowercase();
        let original = res(
            &format!("{}:{VIDEO_ORIGINAL}", dlna_video_mime(&m.mime)),
            &format!(r#" size="{}"{duration}{resolution}"#, m.size),
            &urls.file(m.id, &format!("original.{ext}")),
        );
        let converted = urls.transcode.then(|| {
            res(&format!("video/mpeg:{VIDEO_CONVERTED}"), &duration, &urls.file(m.id, "stream.ts"))
        });
        // TVs play the first entry they support, so put the converted stream first when
        // the original probably won't play.
        // The thumbnail comes right after the first video `res`, before any second video.
        match converted {
            Some(c) if !transcode::tv_plays_original(m) => {
                xml.push_str(&c);
                xml.push_str(&thumb_res);
                xml.push_str(&original);
            }
            Some(c) => {
                xml.push_str(&original);
                xml.push_str(&thumb_res);
                xml.push_str(&c);
            }
            None => {
                xml.push_str(&original);
                xml.push_str(&thumb_res);
            }
        }
    } else {
        let resolution = thumbs::tv_size(m).map(|(w, h)| format!(r#" resolution="{w}x{h}""#)).unwrap_or_default();
        xml.push_str(&res(&format!("image/jpeg:{}", photo_features(m)), &resolution, &urls.file(m.id, "photo.jpg")));
        xml.push_str(&thumb_res);
    }
    xml.push_str("</item>");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn object_ids_round_trip() {
        for id in ["0", "t", "t:2024", "t:2024-03", "a", "a:12", "h", "h:7", "p", "p:IN", "pl:45", "f", "v", "d", "d:3", "d:3:Trip/Day 2", "t:2024-03|77", "d:3:A|B|5"] {
            let obj = Obj::parse(id).unwrap_or_else(|| panic!("{id} parses"));
            assert_eq!(obj.id(), id);
        }
        assert_eq!(Obj::parse("d:3:A|B|5"), Some(Obj::Item("d:3:A|B".into(), 5)));
        for bad in ["", "x", "a:x", "t:20", "1|x", "pl:"] {
            assert_eq!(Obj::parse(bad), None, "{bad}");
        }
        assert_eq!(month_title("2024-03"), "March 2024");
        assert_eq!(month_title("2024-13"), "2024-13");
        assert_eq!(month_query("2024-02").to.as_deref(), Some("2024-02-29"));
        assert_eq!(month_query("2023-02").to.as_deref(), Some("2023-02-28"));
        assert_eq!(month_query("2023-11").to.as_deref(), Some("2023-11-30"));
    }

    #[test]
    fn paging() {
        assert_eq!(page_of(vec![1, 2, 3, 4, 5], 1, 2), (vec![2, 3], 5));
        assert_eq!(page_of(vec![1, 2, 3], 0, 0), (vec![1, 2, 3], 3));
        assert_eq!(page_of(vec![1, 2, 3], 5, 2), (vec![], 3));
    }
}
