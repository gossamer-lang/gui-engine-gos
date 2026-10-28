//! Actual backend readback, recovery and bounded-work rendering measurements.
use gui_engine::geometry::Rect;
use gui_engine::render::Canvas;
use gui_engine::render::presenter::{Painted, WindowRenderer};
use gui_engine::render::retained::Scene;
use gui_engine::style::color::Color;
use gui_engine::text::layout::{self, LayoutOptions, TextStyle};
use std::time::Instant;

fn scene(
    retained: &gui_engine::render::retained::Retained,
    width: i64,
    height: i64,
    caret: bool,
    revision: i64,
    scroll: i64,
) -> Scene {
    let mut c = retained.recording(width, height);
    c.fill(Color::rgb(247, 245, 240));
    c.round_rect(12, 12, 300, 180, 8, Color::rgb(230, 225, 215));
    let style = TextStyle::mono(16.0);
    let clip = c.clip;
    c.push_clip(Rect::new(12, 12, 300, 180));
    for y in 0..6 {
        let source = format!("fn row_{y}() {{ let value = {}; }}", revision + y);
        let text = layout::layout(&source, &style, LayoutOptions::single_line());
        layout::draw(
            &mut c,
            &text,
            20.0,
            (20 + y * 24 - scroll) as f64,
            &style,
            Color::rgb(25, 35, 45),
        );
    }
    c.pop_clip(clip);
    if caret {
        c.rect(315, 20, 1, 18, Color::rgb(25, 35, 45));
    }
    gui_engine::render::display::draw(
        &mut c,
        &[gui_engine::render::display::DrawOp::Layer {
            ops: vec![
                gui_engine::render::display::DrawOp::Opacity(180),
                gui_engine::render::display::DrawOp::RoundRect {
                    rect: Rect::new(0, 0, 16, 12),
                    radius: 3,
                    color: Color::rgb(180, 30, 80),
                },
            ],
            transform: gui_engine::render::retained::Transform {
                x: (width - 30) as f64,
                y: (height - 50) as f64,
                scale_x: -2.0,
                scale_y: 2.0,
            },
        }],
        0,
        0,
    );
    c.take_scene().unwrap()
}
// The pixels `scene` paints when replayed at once: the reference the
// retained, damage-driven frames must match.
fn reference(scene: &Scene, width: i64, height: i64) -> Canvas {
    let mut c = Canvas::new(width, height);
    scene.replay(&mut c);
    c
}

// The compositor may apply a size later than SDL's own synchronization, so
// measurement waits for the window to report the requested pixel size.
fn resize(
    app: &windowing::App,
    window: windowing::Window,
    width: i64,
    height: i64,
) -> Result<(), windowing::Error> {
    window.set_size(width, height)?;
    let _ = window.sync();
    while window.pixel_size() != (width, height) {
        app.wait()?;
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let require_gpu = std::env::args().any(|a| a == "--require-gpu");
    let require_software = std::env::args().any(|a| a == "--require-software");
    assert!(
        !(require_gpu && require_software),
        "conflicting renderer requirements"
    );
    let app = windowing::init("Renderer verification", &windowing::InitOptions::default())?;
    gui_engine::text::fonts::use_system_fonts(false);
    println!(
        "width,height,backend,operation,p95_ns,p99_ns,damage_pixels,rasterized_pixels,cache_bytes"
    );
    for (width, height) in [(640, 480), (1920, 1080), (3840, 2160)] {
        // The resize measurement grows the window by 8 pixels. A desktop
        // that keeps windows on its screen (macOS) cannot give a window more
        // room than the display has, so a size it cannot hold is skipped.
        let window = app.window("Rendering", width + 8, height + 8, true)?;
        let _ = window.sync();
        let held = window.pixel_size();
        if held != (width + 8, height + 8) {
            eprintln!(
                "render-check: skipped {width}x{height}: the display holds a {}x{} window",
                held.0, held.1
            );
            window.close();
            continue;
        }
        resize(&app, window, width, height)?;
        let mut renderer = WindowRenderer::default();
        let first = scene(&renderer.retained, width, height, true, 0, 0);
        let expected = reference(&first, width, height);
        renderer.present(window, width, height, Painted::Scene(first))?;
        if require_software {
            assert!(
                renderer.gpu_name().is_none(),
                "software rendering initialized a GPU"
            );
        }
        if require_gpu {
            assert!(renderer.gpu_name().is_some(), "GPU unavailable");
        }
        if let Some(actual) = renderer.gpu_capture()? {
            assert_eq!(actual.len(), expected.pixels.len());
            let max = actual
                .iter()
                .zip(&expected.pixels)
                .map(|(a, b)| a.abs_diff(*b))
                .max()
                .unwrap_or(0);
            assert!(max <= 8, "GPU comparison: maximum channel difference {max}");
        } else {
            let frame = window.capture()?;
            let (r, g, b) = frame.format.rgb_offsets();
            for y in 0..height {
                for x in 0..width {
                    let src = (y * frame.stride + x * 4) as usize;
                    let dst = ((y * width + x) * 4) as usize;
                    assert_eq!(
                        [
                            frame.pixels[src + r],
                            frame.pixels[src + g],
                            frame.pixels[src + b]
                        ],
                        expected.pixels[dst..dst + 3]
                    );
                }
            }
        }
        for operation in ["caret", "typing", "scroll", "resize", "idle"] {
            if window.pixel_size() != (width, height) {
                resize(&app, window, width, height)?;
            }
            let warm = scene(&renderer.retained, width, height, true, 0, 0);
            renderer.present(window, width, height, Painted::Scene(warm))?;
            let mut times = Vec::new();
            for n in 0..30 {
                let begin = Instant::now();
                let (w, h) = if operation == "resize" {
                    (width + (n % 2) * 8, height + (n % 2) * 8)
                } else {
                    (width, height)
                };
                if window.pixel_size() != (w, h) {
                    resize(&app, window, w, h)?;
                }
                let revision = if operation == "typing" { n } else { 0 };
                let caret = operation != "caret" || n % 2 == 0;
                let frame = scene(
                    &renderer.retained,
                    w,
                    h,
                    caret,
                    revision,
                    if operation == "scroll" { n % 24 } else { 0 },
                );
                let expected = if n == 15 {
                    Some(reference(&frame, w, h))
                } else {
                    None
                };
                renderer.present(window, w, h, Painted::Scene(frame))?;
                if require_software {
                    assert!(
                        renderer.gpu_name().is_none(),
                        "software rendering initialized a GPU during {operation}"
                    );
                }
                times.push(begin.elapsed().as_nanos());
                if require_gpu {
                    assert!(
                        renderer.gpu_name().is_some(),
                        "GPU silently fell back during {operation}"
                    );
                }
                if let Some(expected) = expected
                    && let Some(actual) = renderer.gpu_capture()?
                {
                    let max = actual
                        .iter()
                        .zip(&expected.pixels)
                        .map(|(a, b)| a.abs_diff(*b))
                        .max()
                        .unwrap_or(0);
                    assert!(max <= 8, "incremental GPU {operation}: difference {max}");
                }
                assert!(renderer.retained.stats.layer_bytes <= renderer.retained.budgets.layers);
                assert!(
                    renderer.retained.stats.gpu_layer_bytes <= renderer.retained.budgets.layers
                );
            }
            times.sort_unstable();
            let stats = &renderer.retained.stats;
            if operation == "idle" {
                assert_eq!(stats.damage_pixels, 0);
                assert_eq!(stats.rasterized_pixels, 0);
                assert!(
                    stats.cache_hits > 0,
                    "unchanged transformed subtree was rasterized again"
                );
            }
            if operation == "caret" {
                assert_eq!(stats.damage_pixels, 18);
            }
            println!(
                "{width},{height},{},{operation},{},{},{},{},{}",
                renderer.gpu_name().unwrap_or("software"),
                times[28],
                times[29],
                stats.damage_pixels,
                stats.rasterized_pixels,
                stats.layer_bytes
            );
        }
        app.simulate(windowing::Event::RenderReset {
            window: window.id(),
        })?;
        let mut reset = false;
        for _ in 0..1024 {
            match app.wait_timeout(100)? {
                Some(windowing::Event::RenderReset { window: id }) if id == window.id() => {
                    reset = true;
                    break;
                }
                Some(_) => {}
                None => break,
            }
        }
        assert!(reset, "renderer reset did not survive the event queue");
        // Exercise the same release-and-software fallback as a failed GPU draw.
        renderer.device_lost();
        if window.pixel_size() != (width, height) {
            resize(&app, window, width, height)?;
        }
        let lost = scene(&renderer.retained, width, height, true, 0, 0);
        renderer.present(window, width, height, Painted::Scene(lost))?;
        assert!(
            renderer.gpu_name().is_none(),
            "device-loss frame must use software"
        );
        let _software_frame = window.capture()?;
        // Drop/recreate target and layers, as on renderer-reset notification.
        renderer.invalidate();
        let recovered = scene(&renderer.retained, width, height, true, 0, 0);
        if window.pixel_size() != (width, height) {
            resize(&app, window, width, height)?;
        }
        renderer.present(window, width, height, Painted::Scene(recovered))?;
        if require_gpu {
            assert!(renderer.gpu_name().is_some(), "GPU recovery failed");
        }
        if require_software {
            assert!(
                renderer.gpu_name().is_none(),
                "software rendering initialized a GPU after reset"
            );
        }
        drop(renderer);
        window.close();
    }
    app.finalize();
    Ok(())
}
