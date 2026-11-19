use image::{Rgb, RgbImage};

use super::filters::{self, Group, LOOKS};
use super::render::render;
use super::*;

/// A test picture: horizontal brightness ramp, vertical hue change, a marked top-left corner.
fn picture(w: u32, h: u32) -> RgbImage {
    RgbImage::from_fn(w, h, |x, y| {
        if x < w / 8 && y < h / 8 {
            return Rgb([255, 0, 0]);
        }
        let v = (x * 255 / (w - 1)) as u8;
        let t = y as f32 / h as f32;
        Rgb([v, (v as f32 * (1.0 - t * 0.5)) as u8, (v as f32 * (0.5 + t * 0.5)) as u8])
    })
}

fn mean(img: &RgbImage) -> [f32; 3] {
    let mut s = [0.0f64; 3];
    for p in img.pixels() {
        for k in 0..3 {
            s[k] += p[k] as f64;
        }
    }
    let n = (img.width() * img.height()) as f64;
    [(s[0] / n) as f32, (s[1] / n) as f32, (s[2] / n) as f32]
}

fn max_diff(a: &RgbImage, b: &RgbImage) -> u8 {
    a.as_raw().iter().zip(b.as_raw()).map(|(x, y)| x.abs_diff(*y)).max().unwrap_or(0)
}

#[test]
fn no_edit_is_the_picture_itself() {
    let src = picture(64, 48);
    let out = render(&src, None, &Edit::default(), 1000, 1000);
    assert_eq!((out.width(), out.height()), (64, 48));
    assert!(max_diff(&src, &out) <= 1, "{}", max_diff(&src, &out));
}

#[test]
fn turns_flips_and_crops() {
    let src = picture(80, 40);
    // A quarter turn clockwise: the red top-left corner ends up top-right.
    let out = render(&src, None, &Edit { rotate: 1, ..Default::default() }, 1000, 1000);
    assert_eq!((out.width(), out.height()), (40, 80));
    assert_eq!(out.get_pixel(38, 1).0, [255, 0, 0]);
    assert_ne!(out.get_pixel(1, 1).0, [255, 0, 0]);
    // Flipped: top-right.
    let out = render(&src, None, &Edit { flip: true, ..Default::default() }, 1000, 1000);
    assert_eq!(out.get_pixel(78, 1).0, [255, 0, 0]);
    // Half a turn: bottom-right.
    let out = render(&src, None, &Edit { rotate: 2, ..Default::default() }, 1000, 1000);
    assert_eq!(out.get_pixel(78, 38).0, [255, 0, 0]);
    // Three quarters: bottom-left.
    let out = render(&src, None, &Edit { rotate: 3, ..Default::default() }, 1000, 1000);
    assert_eq!((out.width(), out.height()), (40, 80));
    assert_eq!(out.get_pixel(1, 78).0, [255, 0, 0]);

    // Crop the right half: the left edge of the result is the middle of the picture.
    let crop = Edit { crop: Some(Crop { x: 0.5, y: 0.0, w: 0.5, h: 1.0 }), ..Default::default() };
    let out = render(&src, None, &crop, 1000, 1000);
    assert_eq!((out.width(), out.height()), (40, 40));
    assert_eq!(out.get_pixel(0, 39).0, src.get_pixel(40, 39).0);
    assert_eq!(crop.output_size(80, 40), (40, 40));
    // Size after a turn and a crop, in the turned picture's terms.
    let both = Edit { rotate: 1, crop: Some(Crop { x: 0.0, y: 0.0, w: 1.0, h: 0.5 }), ..Default::default() };
    assert_eq!(both.output_size(80, 40), (40, 40));
}

#[test]
fn straightening_turns_around_the_centre() {
    let src = picture(101, 101);
    let e = Edit { straighten: 10.0, crop: Some(Crop { x: 0.2, y: 0.2, w: 0.6, h: 0.6 }), ..Default::default() };
    let out = render(&src, None, &e, 1000, 1000);
    let c = out.get_pixel(out.width() / 2, out.height() / 2).0;
    let s = src.get_pixel(50, 50).0;
    for k in 0..3 {
        assert!(c[k].abs_diff(s[k]) <= 3, "{c:?} vs {s:?}");
    }
    // Turned clockwise: the row through the centre now climbs to the left, so the ramp
    // (brighter to the right) is brighter above-left than below-left of where it was.
    let left_up = out.get_pixel(5, out.height() / 2 - 8).0[0];
    let unturned = render(&src, None, &Edit { straighten: 0.0, ..e.clone() }, 1000, 1000);
    assert_ne!(left_up, unturned.get_pixel(5, out.height() / 2 - 8).0[0]);
}

#[test]
fn renders_fit_the_box_and_stay_alike_at_every_size() {
    let src = picture(1200, 800);
    let e = Edit {
        adjust: Adjust { shadows: 60.0, highlights: -50.0, clarity: 40.0, dehaze: 30.0, ..Default::default() },
        filter: Some("night800".into()),
        ..Default::default()
    };
    let big = render(&src, None, &e, 1200, 1200);
    let small = render(&src, None, &e, 300, 300);
    assert_eq!((small.width(), small.height()), (300, 200));
    let shrunk = image::imageops::thumbnail(&big, 300, 200);
    let (a, b) = (mean(&small), mean(&shrunk));
    for k in 0..3 {
        assert!((a[k] - b[k]).abs() < 4.0, "{a:?} vs {b:?}");
    }
}

#[test]
fn sliders_do_what_they_say() {
    let src = picture(64, 64);
    let base = mean(&src);
    let with = |a: Adjust| mean(&render(&src, None, &Edit { adjust: a, ..Default::default() }, 64, 64));

    let brighter = with(Adjust { exposure: 50.0, ..Default::default() });
    assert!(brighter[1] > base[1] + 10.0);
    let darker = with(Adjust { exposure: -50.0, ..Default::default() });
    assert!(darker[1] < base[1] - 10.0);
    let warm = with(Adjust { temperature: 60.0, ..Default::default() });
    assert!(warm[0] - warm[2] > base[0] - base[2] + 5.0);
    let lifted = with(Adjust { shadows: 80.0, ..Default::default() });
    assert!(lifted[1] > base[1]);

    let grey = render(&src, None, &Edit { adjust: Adjust { saturation: -100.0, ..Default::default() }, ..Default::default() }, 64, 64);
    for p in grey.pixels() {
        assert!(p[0].abs_diff(p[1]) <= 2 && p[1].abs_diff(p[2]) <= 2, "{p:?}");
    }
    // Contrast keeps middle grey and spreads the ends.
    let mid = RgbImage::from_pixel(8, 8, Rgb([128, 128, 128]));
    let out = render(&mid, None, &Edit { adjust: Adjust { contrast: 80.0, ..Default::default() }, ..Default::default() }, 8, 8);
    assert!(out.get_pixel(0, 0)[0].abs_diff(128) <= 4);
    // Vignette darkens corners, not the centre.
    let flat = RgbImage::from_pixel(64, 64, Rgb([180, 180, 180]));
    let v = render(&flat, None, &Edit { adjust: Adjust { vignette: -80.0, ..Default::default() }, ..Default::default() }, 64, 64);
    assert!(v.get_pixel(0, 0)[0] < 150);
    assert!(v.get_pixel(32, 32)[0] >= 178);
}

#[test]
fn every_filter_renders_and_strength_zero_does_nothing() {
    let src = picture(96, 64);
    let plain = render(&src, None, &Edit::default(), 96, 64);
    for look in LOOKS {
        let e = Edit { filter: Some(look.id.into()), ..Default::default() };
        let out = render(&src, None, &e, 96, 64);
        assert!(max_diff(&out, &plain) > 5, "{} changes nothing", look.id);
        if look.group == Group::Mono && look.shadows.amount == 0.0 && look.highlights.amount == 0.0 {
            for p in out.pixels() {
                assert!(p[0].abs_diff(p[1]) <= 3 && p[1].abs_diff(p[2]) <= 3, "{}: {p:?}", look.id);
            }
        }
        let none = render(&src, None, &Edit { filter_strength: 0.0, ..e.clone() }, 96, 64);
        assert!(max_diff(&none, &plain) <= 1, "{}", look.id);
    }
    assert!(filters::find("nope").is_none());
}

#[test]
fn background_blur_keeps_the_subject_sharp() {
    // Fine stripes everywhere; the mask says the left half is the subject.
    let src = RgbImage::from_fn(128, 64, |x, _| if x % 2 == 0 { Rgb([250, 250, 250]) } else { Rgb([10, 10, 10]) });
    let mask = image::GrayImage::from_fn(128, 64, |x, _| image::Luma([if x < 64 { 255 } else { 0 }]));
    let out = render(&src, Some(&mask), &Edit { blur: 100.0, ..Default::default() }, 128, 64);
    let contrast = |x0: u32| (x0..x0 + 8).map(|x| out.get_pixel(x, 32)[0]).max().unwrap() - (x0..x0 + 8).map(|x| out.get_pixel(x, 32)[0]).min().unwrap();
    assert!(contrast(16) > 200, "subject stays sharp");
    assert!(contrast(100) < 40, "background is blurred");
    // Without a mask there is no blur.
    let plain = render(&src, None, &Edit { blur: 100.0, ..Default::default() }, 128, 64);
    assert_eq!(plain.as_raw(), src.as_raw());
}

#[test]
fn enhance_brightens_a_dark_photo_and_fixes_a_cast() {
    let dark = RgbImage::from_fn(64, 64, |x, y| {
        let v = ((x + y) / 4) as u8 + 10;
        Rgb([v, v, (v as f32 * 1.6) as u8])
    });
    let a = auto::enhance(&dark, &Edit::default(), None);
    assert!(a.exposure > 10.0, "{a:?}");
    assert!(a.temperature > 0.0, "a blue cast is warmed: {a:?}");
    let out = render(&dark, None, &Edit { adjust: a, ..Default::default() }, 64, 64);
    let (before, after) = (mean(&dark), mean(&out));
    assert!(after[1] > before[1] + 10.0, "{a:?} {before:?} -> {after:?}");
    assert!(after[2] / after[0] < before[2] / before[0], "{before:?} -> {after:?}");
}

#[test]
fn recipes_are_cleaned_and_identity_is_detected() {
    let e = Edit {
        rotate: 7,
        straighten: 90.0,
        crop: Some(Crop { x: -1.0, y: 0.0, w: 3.0, h: 1.0 }),
        adjust: Adjust { exposure: f32::NAN, grain: -5.0, contrast: 500.0, ..Default::default() },
        filter: Some("unknown".into()),
        filter_strength: 4.0,
        ..Default::default()
    }
    .normalized();
    assert_eq!(e.rotate, 3);
    assert_eq!(e.straighten, 45.0);
    assert_eq!(e.crop, None, "a crop of the whole picture is no crop");
    assert_eq!((e.adjust.exposure, e.adjust.grain, e.adjust.contrast), (0.0, 0.0, 100.0));
    assert_eq!((e.filter.as_deref(), e.filter_strength), (None, 1.0));
    assert!(Edit::default().is_identity());
    assert!(!e.is_identity());
    let json = r#"{"adjust":{"exposure":10},"future":"ignored"}"#;
    assert_eq!(parse(json).unwrap().adjust.exposure, 10.0);
    assert!(parse(r#"{"filterStrength":0.5}"#).is_none());
}

#[test]
fn saving_reverting_and_sizes() {
    let (_dir, db) = crate::db::test_db();
    let conn = db.conn().unwrap();
    conn.execute("INSERT INTO folders (path, added_at) VALUES ('C:\\P', '')", []).unwrap();
    conn.execute(
        "INSERT INTO media (folder_id, path, name, size, mtime, kind, mime, width, height, taken_at, stage, added_at)
         VALUES (1, 'C:\\P\\a.jpg', 'a.jpg', 1, 5000, 'photo', 'image/jpeg', 4000, 3000, '2024-01-01T10:00:00', 3, ''),
                (1, 'C:\\P\\v.mp4', 'v.mp4', 1, 5000, 'video', 'video/mp4', 1920, 1080, '2024-01-01T11:00:00', 3, '')",
        [],
    )
    .unwrap();
    drop(conn);
    let e = Edit { rotate: 1, crop: Some(Crop { x: 0.0, y: 0.0, w: 0.5, h: 1.0 }), ..Default::default() };
    let saved = save(&db, 1, Some(e.clone())).unwrap();
    assert_eq!(saved.edit, Some(e.clone()));
    let first = saved.edited_at.unwrap();
    let m = crate::media::get(&db, 1).unwrap();
    assert_eq!(m.edit, Some(e));
    assert_eq!(m.shown_size(), Some((1500, 4000)));
    assert_eq!(m.version(), first);
    let items = crate::media::timeline(&db).unwrap();
    let it = items.iter().find(|i| i.id == 1).unwrap();
    assert_eq!((it.w, it.h, it.m), (1500, 4000, first / 1000));
    assert_eq!(edited_among(&db, &[1, 2]).unwrap(), vec![1]);

    // Reverting keeps a newer version (so pictures are made again), at least a second later.
    let reverted = save(&db, 1, Some(Edit::default())).unwrap();
    assert_eq!(reverted.edit, None);
    assert!(reverted.edited_at.unwrap() >= first + 1000);
    let m = crate::media::get(&db, 1).unwrap();
    assert_eq!((m.edit.as_ref(), m.shown_size()), (None, Some((4000, 3000))));
    assert!(edited_among(&db, &[1]).unwrap().is_empty());

    assert!(save(&db, 2, Some(Edit { rotate: 1, ..Default::default() })).is_err(), "videos can't be edited");
    assert!(matches!(save(&db, 99, None), Err(Error::NotFound)));
}

#[test]
fn edited_pictures_are_made_from_the_edit() {
    let dir = tempfile::tempdir().unwrap();
    let paths = crate::AppPaths::in_dir(&dir.path().join("data")).unwrap();
    let file = dir.path().join("p.png");
    picture(300, 200).save(&file).unwrap();
    let mut m = crate::media::Media::for_test(&file, "image/png");
    (m.width, m.height, m.mtime) = (Some(300), Some(200), 1);
    let plain = crate::thumbs::ensure_display(&paths, &m).unwrap();
    assert_eq!(plain, file, "unedited PNGs are shown as they are");

    m.edit = Some(Edit { rotate: 1, filter: Some("mono".into()), ..Default::default() });
    m.edited_at = Some(chrono::Utc::now().timestamp_millis());
    let shown = crate::thumbs::ensure_display(&paths, &m).unwrap();
    assert_ne!(shown, file);
    let img = image::open(&shown).unwrap().to_rgb8();
    assert_eq!((img.width(), img.height()), (200, 300));
    let p = img.get_pixel(150, 100);
    assert!(p[0].abs_diff(p[2]) <= 3, "black and white: {p:?}");
    let thumb = image::open(crate::thumbs::ensure_thumb(&paths, &m).unwrap()).unwrap();
    assert_eq!((thumb.width(), thumb.height()), (200, 300), "never upscaled");
}

#[test]
fn enhance_leaves_a_well_exposed_photo_with_a_bright_sky_alone() {
    // Bright sky above, a mid-toned subject in the middle: already fine.
    let img = RgbImage::from_fn(90, 120, |x, y| {
        if y < 50 { Rgb([225, 230, 238]) } else { let v = 90 + ((x + y) % 60) as u8; Rgb([v + 20, v, v - 10]) }
    });
    let a = auto::enhance(&img, &Edit::default(), Some(filters::Scene::Portrait));
    assert!(a.exposure.abs() <= 10.0, "{a:?}");
    assert!(a.whites <= 20.0, "{a:?}");
}

#[test]
fn enhance_keeps_snow_bright_and_a_sunset_warm() {
    // Snow, slightly blue in shade, under a darker band of trees.
    let snow = RgbImage::from_fn(90, 120, |x, y| {
        if y < 30 { Rgb([60, 70, 60]) } else { let v = 205 + ((x + y) % 40) as u8; Rgb([v - 12, v - 4, v]) }
    });
    let plain = auto::enhance(&snow, &Edit::default(), None);
    let as_snow = auto::enhance(&snow, &Edit::default(), Some(filters::Scene::Snow));
    assert!(as_snow.exposure >= plain.exposure, "{plain:?} / {as_snow:?}");
    assert!(as_snow.exposure >= -5.0, "snow is not darkened: {as_snow:?}");
    assert!(as_snow.highlights >= -25.0, "{as_snow:?}");
    assert!(as_snow.temperature > 0.0, "blue shade is corrected: {as_snow:?}");

    // An orange sky over dark water.
    let sunset = RgbImage::from_fn(90, 120, |x, y| {
        if y < 70 { Rgb([230, 140 + (x % 20) as u8, 70]) } else { Rgb([50, 35, 30]) }
    });
    let plain = auto::enhance(&sunset, &Edit::default(), None);
    let as_sunset = auto::enhance(&sunset, &Edit::default(), Some(filters::Scene::Sunset));
    assert!(as_sunset.temperature >= plain.temperature, "less of the warmth is taken out: {plain:?} / {as_sunset:?}");
    assert!(as_sunset.temperature.abs() <= plain.temperature.abs(), "{plain:?} / {as_sunset:?}");
}

/// Render timings: `cargo test -p photos-core render_speed -- --ignored --nocapture`
#[test]
#[ignore]
fn render_speed() {
    let src = picture(2560, 1920);
    let cases: [(&str, Edit); 5] = [
        ("plain", Edit::default()),
        ("exposure+contrast", Edit { adjust: Adjust { exposure: 20.0, contrast: 20.0, ..Default::default() }, ..Default::default() }),
        ("vibrance", Edit { adjust: Adjust { vibrance: 30.0, ..Default::default() }, ..Default::default() }),
        ("tone maps", Edit { adjust: Adjust { shadows: 40.0, highlights: -40.0, clarity: 30.0, ..Default::default() }, ..Default::default() }),
        ("filter", Edit { filter: Some("portrait400".into()), ..Default::default() }),
    ];
    for (name, e) in cases {
        render(&src, None, &e, 1600, 1600);
        let t = std::time::Instant::now();
        for _ in 0..5 {
            render(&src, None, &e, 1600, 1600);
        }
        println!("{name}: {:.1} ms", t.elapsed().as_secs_f64() * 1000.0 / 5.0);
    }
}

#[test]
fn every_blur_style_keeps_the_subject_sharp() {
    let src = RgbImage::from_fn(160, 96, |x, y| if (x + y) % 2 == 0 { Rgb([240, 240, 240]) } else { Rgb([20, 20, 20]) });
    let mask = image::GrayImage::from_fn(160, 96, |x, _| image::Luma([if x < 60 { 255 } else { 0 }]));
    for style in [BlurStyle::Lens, BlurStyle::Oval, BlurStyle::Soft, BlurStyle::Swirl, BlurStyle::Motion, BlurStyle::Zoom] {
        let out = render(&src, Some(&mask), &Edit { blur: 100.0, blur_style: style, ..Default::default() }, 160, 96);
        let spread = |x0: u32, y: u32| {
            let v: Vec<u8> = (x0..x0 + 6).map(|x| out.get_pixel(x, y)[0]).collect();
            v.iter().max().unwrap() - v.iter().min().unwrap()
        };
        assert!(spread(20, 48) > 200, "{style:?}: subject stays sharp");
        assert!(spread(130, 20) < 120, "{style:?}: background is blurred ({})", spread(130, 20));
    }
}

#[test]
fn motion_blur_runs_sideways_and_lens_blur_makes_discs() {
    // Vertical stripes vanish with sideways streaks; horizontal ones stay.
    let mask = image::GrayImage::new(120, 120);
    let vertical = RgbImage::from_fn(120, 120, |x, _| if (x / 3) % 2 == 0 { Rgb([230, 230, 230]) } else { Rgb([30, 30, 30]) });
    let horizontal = RgbImage::from_fn(120, 120, |_, y| if (y / 3) % 2 == 0 { Rgb([230, 230, 230]) } else { Rgb([30, 30, 30]) });
    let motion = Edit { blur: 80.0, blur_style: BlurStyle::Motion, ..Default::default() };
    let contrast = |img: &RgbImage, vertical_line: bool| {
        let v: Vec<u8> = (40..80).map(|t| if vertical_line { img.get_pixel(60, t)[0] } else { img.get_pixel(t, 60)[0] }).collect();
        v.iter().max().unwrap() - v.iter().min().unwrap()
    };
    assert!(contrast(&render(&vertical, Some(&mask), &motion, 120, 120), false) < 60);
    assert!(contrast(&render(&horizontal, Some(&mask), &motion, 120, 120), true) > 120);

    // One bright point on a dark background: a lens spreads it into a flat disc with a
    // clear edge; a Gaussian into a blob that fades from the middle.
    let mut dot = RgbImage::from_pixel(200, 200, Rgb([10, 10, 10]));
    for y in 98..102 {
        for x in 98..102 {
            dot.put_pixel(x, y, Rgb([255, 255, 255]));
        }
    }
    let mask = image::GrayImage::new(200, 200);
    let profile = |style| {
        let out = render(&dot, Some(&mask), &Edit { blur: 100.0, blur_style: style, ..Default::default() }, 200, 200);
        (out.get_pixel(100, 100)[0] as f32, out.get_pixel(103, 100)[0] as f32)
    };
    let (lc, lmid) = profile(BlurStyle::Lens);
    let (sc, smid) = profile(BlurStyle::Soft);
    assert!(lmid / lc > smid / sc, "lens disc is flatter: lens {lc}/{lmid}, soft {sc}/{smid}");
}

/// `cargo test -p photos-core blur_speed -- --ignored --nocapture`
#[test]
#[ignore]
fn blur_speed() {
    // The editor renders from the copy nearest the wanted size (see `session::Source`).
    let (full, mid) = (picture(2560, 1920), picture(1280, 960));
    let mask = image::GrayImage::from_fn(1024, 768, |x, _| image::Luma([if x < 400 { 255 } else { 0 }]));
    for style in [BlurStyle::Lens, BlurStyle::Oval, BlurStyle::Soft, BlurStyle::Swirl, BlurStyle::Motion, BlurStyle::Zoom] {
        for (size, amount) in [(1600, 100.0), (900, 100.0)] {
            let src = if size > 1280 { &full } else { &mid };
            let e = Edit { blur: amount, blur_style: style, ..Default::default() };
            render(src, Some(&mask), &e, size, size);
            let t = std::time::Instant::now();
            for _ in 0..3 {
                render(src, Some(&mask), &e, size, size);
            }
            println!("{style:?} {size}: {:.1} ms", t.elapsed().as_secs_f64() * 1000.0 / 3.0);
        }
    }
}
