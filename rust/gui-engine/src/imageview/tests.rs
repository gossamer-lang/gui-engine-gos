use super::*;

const PLAIN: &[u8] = include_bytes!("fixtures/plain.jpg");
const ROTATED: &[u8] = include_bytes!("fixtures/rotated.jpg");
const GIF: &[u8] = include_bytes!("fixtures/small.gif");

fn pixel(image: &Image, x: i64, y: i64) -> [u8; 4] {
    let at = ((y * image.width + x) * 4) as usize;
    [
        image.rgba[at],
        image.rgba[at + 1],
        image.rgba[at + 2],
        image.rgba[at + 3],
    ]
}

fn reddish(p: [u8; 4]) -> bool {
    p[0] > 180 && p[2] < 80
}

fn bluish(p: [u8; 4]) -> bool {
    p[2] > 180 && p[0] < 80
}

fn png_bytes(width: i64, height: i64, rgba: &[u8]) -> Vec<u8> {
    crate::render::png::encode(width, height, rgba).unwrap()
}

#[test]
fn every_supported_format_decodes_and_jpeg_orientation_is_applied() {
    let plain = decode(PLAIN, &|| false).unwrap();
    assert_eq!(
        (plain.width, plain.height, plain.format),
        (64, 32, Format::Jpeg)
    );
    assert!(reddish(pixel(&plain, 4, 16)) && bluish(pixel(&plain, 60, 16)));
    // Orientation 6: the stored image is turned a quarter clockwise to view.
    let turned = decode(ROTATED, &|| false).unwrap();
    assert_eq!((turned.width, turned.height), (32, 64));
    assert!(reddish(pixel(&turned, 16, 4)) && bluish(pixel(&turned, 16, 60)));
    let gif = decode(GIF, &|| false).unwrap();
    assert_eq!((gif.width, gif.height, gif.format), (40, 20, Format::Gif));
    assert!(pixel(&gif, 10, 10)[1] > 100);
    let png = decode(&png_bytes(3, 2, &[9; 24]), &|| false).unwrap();
    assert_eq!((png.width, png.height, png.format), (3, 2, Format::Png));
    let svg = decode(b"<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 10 10'><rect width='10' height='10' fill='#00ff00'/></svg>", &|| false).unwrap();
    assert_eq!(svg.format, Format::Svg);
    assert!(pixel(&svg, svg.width / 2, svg.height / 2)[1] > 200);
    for turns in 0..4 {
        let back = rotate(&rotate(&plain, turns), 4 - turns);
        assert_eq!(*back.rgba, *plain.rgba);
    }
}

#[test]
fn malformed_oversized_and_cancelled_images_fail_with_reasons() {
    assert!(
        decode(b"not an image at all", &|| false)
            .unwrap_err()
            .contains("not a PNG")
    );
    assert!(decode(&PLAIN[..PLAIN.len() / 2], &|| false).is_err());
    let png = png_bytes(4, 4, &[0; 64]);
    assert!(decode(&png[..png.len() - 20], &|| false).is_err());
    // A header claiming 20000 x 20000 is refused before any pixel exists.
    let mut huge = png.clone();
    huge[16..20].copy_from_slice(&20_000u32.to_be_bytes());
    huge[20..24].copy_from_slice(&20_000u32.to_be_bytes());
    let crc = crc32(&huge[12..29]);
    huge[29..33].copy_from_slice(&crc.to_be_bytes());
    let refused = decode(&huge, &|| false).unwrap_err();
    assert!(refused.contains("beyond the viewer"), "{refused}");
    assert_eq!(decode(PLAIN, &|| true).unwrap_err(), "decoding cancelled");
    assert!(decode(b"<svg", &|| false).is_err());
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for b in bytes {
        crc ^= u32::from(*b);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

#[test]
fn large_images_get_a_reduced_copy_for_small_views() {
    let image = Image {
        width: 5000,
        height: 1000,
        rgba: Pixels::new(vec![200; 5000 * 1000 * 4]),
        format: Format::Png,
    };
    let reduced = preview(&image, 2048).unwrap();
    assert_eq!((reduced.width, reduced.height), (2048, 410));
    assert_eq!(pixel(&reduced, 1000, 200), [200, 200, 200, 200]);
    assert!(
        preview(
            &Image {
                width: 10,
                height: 10,
                rgba: Pixels::new(vec![0; 400]),
                format: Format::Png
            },
            2048
        )
        .is_none()
    );
}

mod session {
    use super::*;
    use crate::widgets::Widget;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn ready(events: &[(i64, Status)], id: i64) -> Option<Status> {
        events
            .iter()
            .rev()
            .find(|(v, _)| *v == id)
            .map(|(_, s)| s.clone())
    }

    #[test]
    fn viewers_load_in_the_background_supersede_reload_replaced_files_and_zoom() {
        crate::testing::on_ui_thread(|| {
            let config = crate::services::private_test_config();
            let _defer = scopeguard::guard((), |_| {
                crate::services::remove_test_config(&config);
            });
            let dir = std::env::temp_dir().join(format!("gui-images-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let photo = dir.join("photo.jpg");
            let other = dir.join("other.gif");
            std::fs::write(&photo, ROTATED).unwrap();
            std::fs::write(&other, GIF).unwrap();
            let mut s = crate::app::start("Images", "dev.gossamer.gui-image-test").unwrap();
            let id = open(crate::app::effects::current_budget()).unwrap();
            let events = Rc::new(RefCell::new(Vec::new()));
            let heard = events.clone();
            s.set_shell(
                move |m| {
                    if let UiMsg::Image(viewer, status) = m {
                        heard.borrow_mut().push((viewer, status));
                    }
                },
                move |_| {
                    Widget::ImageView(widget::View {
                        id: "viewer".into(),
                        viewer: id,
                        label: "Photo".into(),
                        width: 400,
                        height: 300,
                    })
                },
                |_| Vec::new(),
            );
            s.open_window("main", "Images", 400, 300).unwrap();
            // A second load supersedes the first; only its outcome is reported.
            load(id, &other.display().to_string()).unwrap();
            load(id, &photo.display().to_string()).unwrap();
            while !matches!(
                ready(&events.borrow(), id).map(|s| s.state),
                Some(State::Ready { .. } | State::Failed(_))
            ) {
                s.step().unwrap();
            }
            assert_eq!(events.borrow().len(), 1, "{:?}", events.borrow());
            let status = status_of(id).unwrap();
            assert_eq!(
                status.state,
                State::Ready {
                    width: 32,
                    height: 64,
                    format: Format::Jpeg
                }
            );
            let (cached, entries) = cache_usage();
            assert!(cached > 0 && entries >= 1);
            // A replaced file (its length differs) is decoded afresh, not served
            // from the cache.
            std::fs::write(&photo, PLAIN).unwrap();
            assert!(reload(id).unwrap());
            while !matches!(
                ready(&events.borrow(), id).map(|s| s.state),
                Some(State::Ready { width: 64, .. })
            ) {
                s.step().unwrap();
            }
            // A file that is not an image fails with a reason.
            let broken = dir.join("broken.png");
            std::fs::write(&broken, b"\x89PNG\r\n\x1a\nbroken").unwrap();
            load(id, &broken.display().to_string()).unwrap();
            while !matches!(
                ready(&events.borrow(), id).map(|s| s.state),
                Some(State::Failed(_))
            ) {
                s.step().unwrap();
            }
            load(id, &photo.display().to_string()).unwrap();
            while !matches!(
                ready(&events.borrow(), id).map(|s| s.state),
                Some(State::Ready { .. })
            ) {
                s.step().unwrap();
            }
            // Keyboard and accessibility.
            assert!(s.focus("viewer").unwrap());
            s.press(windowing::Key::Num1, windowing::Mods::default())
                .unwrap();
            assert_eq!(status_of(id).unwrap().zoom, Zoom::Scale(1.0));
            s.press(windowing::Key::PageUp, windowing::Mods::default())
                .unwrap();
            assert!(matches!(status_of(id).unwrap().zoom, Zoom::Scale(z) if z > 1.2));
            s.press(windowing::Key::R, windowing::Mods::default())
                .unwrap();
            assert_eq!(
                status_of(id).unwrap().state,
                State::Ready {
                    width: 32,
                    height: 64,
                    format: Format::Jpeg
                }
            );
            s.press(windowing::Key::Home, windowing::Mods::default())
                .unwrap();
            assert_eq!(status_of(id).unwrap().zoom, Zoom::Fit);
            let tree = s.semantics(0);
            fn find<'a>(
                n: &'a crate::accessibility::AccessNode,
                id: &str,
            ) -> Option<&'a crate::accessibility::AccessNode> {
                if n.id == id {
                    return Some(n);
                }
                n.children.iter().find_map(|c| find(c, id))
            }
            let viewer = find(&tree, "viewer").unwrap();
            assert!(
                viewer.description.starts_with("32 by 64 pixels"),
                "{}",
                viewer.description
            );
            s.shutdown();
            assert!(status_of(id).is_err());
            std::fs::remove_dir_all(dir).unwrap();
        })
    }
}
