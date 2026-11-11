//! Group all automatically placed faces again, on a copy of a library database:
//! `cargo run -p photos-ml --example recluster -- <library.db> [join merge min_group]`
fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let db = photos_core::Db::open(std::path::Path::new(&a[0])).unwrap();
    let mut p = photos_core::people::ClusterParams::default();
    if a.len() >= 4 {
        (p.join, p.merge, p.min_group) = (a[1].parse().unwrap(), a[2].parse().unwrap(), a[3].parse().unwrap());
    }
    db.conn()
        .unwrap()
        .execute_batch(
            "UPDATE faces SET person_id = NULL WHERE locked = 0;
             DELETE FROM persons WHERE id NOT IN (SELECT person_id FROM faces WHERE person_id IS NOT NULL);",
        )
        .unwrap();
    let t = std::time::Instant::now();
    let first = photos_core::people::cluster_with(&db, &p).unwrap();
    let again = photos_core::people::cluster_with(&db, &p).unwrap();
    println!("{p:?} in {:.1?}: {first:?} then {again:?}", t.elapsed());
}
