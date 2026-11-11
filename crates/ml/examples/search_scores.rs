//! Score distribution of content search: `cargo run -p photos-ml --release --example search_scores -- <models> <library.db> <query>...`
use photos_ml::clip::TextEncoder;
use photos_ml::{CLIP_TEXT, CLIP_TOKENIZER, open_session};

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let models = std::path::PathBuf::from(&a[0]);
    let db = photos_core::Db::open(std::path::Path::new(&a[1])).unwrap();
    let (s, _) = open_session(&models.join(CLIP_TEXT), false, &[]).unwrap();
    let mut text = TextEncoder::new(s, &models.join(CLIP_TOKENIZER)).unwrap();
    let conn = db.conn().unwrap();
    let mut stmt = conn.prepare("SELECT media_id, vec FROM clip_embeddings").unwrap();
    let rows: Vec<(i64, Vec<f32>)> =
        stmt.query_map([], |r| Ok((r.get(0)?, photos_core::people::from_blob(&r.get::<_, Vec<u8>>(1)?)))).unwrap().map(|r| r.unwrap()).collect();
    let base = text.embed("").unwrap();
    for q in &a[2..] {
        let v = text.embed(q).unwrap();
        let mut s: Vec<(f32, i64)> =
            rows.iter().map(|(id, e)| (photos_core::people::dot(&v, e) - photos_core::people::dot(&base, e), *id)).collect();
        s.sort_by(|a, b| b.0.total_cmp(&a.0));
        let at = |i: usize| s.get(i).map(|x| x.0).unwrap_or(0.0);
        println!("{q:>16}: top {:.3} {:.3} {:.3} | #5 {:.3} #10 {:.3} #20 {:.3} #50 {:.3} median {:.3} | ids {:?}",
            at(0), at(1), at(2), at(5), at(10), at(20), at(50), at(s.len() / 2), s.iter().take(4).map(|x| x.1).collect::<Vec<_>>());
    }
}
