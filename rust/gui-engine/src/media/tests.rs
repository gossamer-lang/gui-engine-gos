use super::*;
use crate::widgets::Widget;
use std::cell::RefCell;
use std::rc::Rc;

fn clip() -> String {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/media/fixtures/clip.webm")
        .display()
        .to_string()
}

fn threads() -> usize {
    std::fs::read_dir("/proc/self/task")
        .map(|d| d.count())
        .unwrap_or(0)
}

/// Runs the session until `done` holds for the player's reported statuses.
fn step_until(
    s: &mut crate::Session,
    heard: &Rc<RefCell<Vec<Status>>>,
    mut done: impl FnMut(&[Status]) -> bool,
) {
    while !done(&heard.borrow()) {
        assert!(s.step().unwrap(), "the session ended");
    }
}

#[test]
fn gstreamer_plays_seeks_reports_progress_fails_cleanly_and_tears_down() {
    crate::testing::on_ui_thread(|| {
        if gst::api().is_err() {
            assert!(
                std::env::var_os("GUI_REQUIRE_GSTREAMER").is_none(),
                "GStreamer is required: {:?}",
                gst::api().err()
            );
            return;
        }
        let config = crate::services::private_test_config();
        let _defer = scopeguard::guard((), |_| crate::services::remove_test_config(&config));
        let mut s = crate::app::start("Media", "dev.gossamer.gui-media-test").unwrap();
        let id = open(AudioOutput::Discard, crate::app::effects::current_budget()).unwrap();
        let heard = Rc::new(RefCell::new(Vec::new()));
        let log = heard.clone();
        s.set_shell(
            move |m| {
                if let UiMsg::Media(_, status) = m {
                    log.borrow_mut().push(status);
                }
            },
            move |_| {
                Widget::Media(widget::View {
                    id: "video".into(),
                    player: id,
                    label: "Clip".into(),
                    width: 320,
                    height: 240,
                })
            },
            Vec::new,
        );
        s.open_window("main", "Media", 320, 240).unwrap();
        load(id, &clip()).unwrap();
        // Loading prerolls the first frame, paused.
        step_until(&mut s, &heard, |h| {
            h.iter()
                .any(|st| st.state == State::Paused && st.frames > 0)
        });
        let ready = status(id).unwrap();
        assert_eq!((ready.width, ready.height), (160, 120));
        assert!((1900..=2100).contains(&ready.duration_ms), "{ready:?}");
        // Playing reports progress until the end.
        play(id).unwrap();
        step_until(&mut s, &heard, |h| {
            h.iter().any(|st| st.state == State::Ended)
        });
        let progress: Vec<i64> = heard
            .borrow()
            .iter()
            .filter(|st| st.state == State::Playing)
            .map(|st| st.position_ms)
            .collect();
        assert!(
            progress.len() >= 3 && progress.windows(2).all(|w| w[1] >= w[0]),
            "{progress:?}"
        );
        assert!(status(id).unwrap().frames >= 15);
        // Seeking back pauses at the new place; volume and mute stick.
        seek(id, 1000).unwrap();
        pause(id).unwrap();
        heard.borrow_mut().clear();
        step_until(&mut s, &heard, |h| {
            h.iter().any(|st| st.state == State::Paused)
        });
        let at = status(id).unwrap().position_ms;
        assert!((900..=1100).contains(&at), "{at}");
        set_volume(id, 0.25).unwrap();
        set_muted(id, true).unwrap();
        let st = status(id).unwrap();
        assert_eq!((st.volume, st.muted), (0.25, true));
        // The accessible surface says what plays.
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
        assert!(
            find(&tree, "video")
                .unwrap()
                .description
                .starts_with("Paused at 0:01"),
            "{:?}",
            find(&tree, "video").map(|n| n.description.clone())
        );
        // A missing file or a folder is refused at once; a broken file fails
        // with a reason.
        let missing = load(id, "/nonexistent/clip.webm").unwrap_err();
        assert!(
            missing.starts_with("cannot read /nonexistent/clip.webm"),
            "{missing}"
        );
        let folder = std::env::temp_dir().display().to_string();
        assert_eq!(
            load(id, &folder).unwrap_err(),
            format!("{folder} is a folder")
        );
        let broken =
            std::env::temp_dir().join(format!("gui-media-broken-{}.webm", std::process::id()));
        std::fs::write(&broken, b"not a media file at all").unwrap();
        heard.borrow_mut().clear();
        load(id, &broken.display().to_string()).unwrap();
        step_until(&mut s, &heard, |h| {
            h.iter().any(|st| matches!(st.state, State::Failed(_)))
        });
        std::fs::remove_file(&broken).unwrap();
        close(id);
        // Repeated players start and stop without leaving threads behind.
        let before = threads();
        for _ in 0..8 {
            let player = open(AudioOutput::Discard, crate::app::effects::current_budget()).unwrap();
            load(player, &clip()).unwrap();
            play(player).unwrap();
            close(player);
        }
        assert!(
            threads() <= before + 2,
            "{} threads, was {before}",
            threads()
        );
        assert!(status(id).is_err());
        s.shutdown();
    })
}
