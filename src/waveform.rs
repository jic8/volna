use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use adw::prelude::*;
use gst::prelude::*;
use gtk::glib;

const SAMPLE_RATE: usize = 8000;
/// 100 значений RMS на секунду аудио
const BUCKET: usize = SAMPLE_RATE / 100;
const BAR_WIDTH: f64 = 2.0;
const BAR_STEP: f64 = 5.0;

/// Декодирует файл в фоновом потоке и возвращает огибающую (RMS).
pub fn analyze(uri: String, cancel: Arc<AtomicBool>) -> async_channel::Receiver<Vec<f32>> {
    let (tx, rx) = async_channel::bounded(1);
    std::thread::spawn(move || {
        let peaks = decode_peaks(&uri, &cancel).unwrap_or_else(|err| {
            eprintln!("waveform: {err}");
            Vec::new()
        });
        let _ = tx.send_blocking(peaks);
    });
    rx
}

fn decode_peaks(uri: &str, cancel: &AtomicBool) -> Result<Vec<f32>, Box<dyn std::error::Error>> {
    let pipeline = gst::Pipeline::new();
    let src = gst::ElementFactory::make("uridecodebin")
        .property("uri", uri)
        .build()?;
    let convert = gst::ElementFactory::make("audioconvert").build()?;
    let resample = gst::ElementFactory::make("audioresample").build()?;
    let caps = gst::Caps::builder("audio/x-raw")
        .field("format", "F32LE")
        .field("layout", "interleaved")
        .field("channels", 1i32)
        .field("rate", SAMPLE_RATE as i32)
        .build();
    let sink = gst_app::AppSink::builder().caps(&caps).sync(false).build();

    pipeline.add_many([&src, &convert, &resample, sink.upcast_ref()])?;
    gst::Element::link_many([&convert, &resample, sink.upcast_ref()])?;

    let convert_weak = convert.downgrade();
    src.connect_pad_added(move |_, pad| {
        let Some(convert) = convert_weak.upgrade() else {
            return;
        };
        let is_audio = pad
            .current_caps()
            .and_then(|caps| caps.structure(0).map(|s| s.name().starts_with("audio/")))
            .unwrap_or(false);
        let sink_pad = convert.static_pad("sink").unwrap();
        if is_audio && !sink_pad.is_linked() {
            let _ = pad.link(&sink_pad);
        }
    });

    pipeline.set_state(gst::State::Playing)?;
    let bus = pipeline.bus().unwrap();

    let mut peaks = Vec::new();
    let (mut acc, mut count) = (0f32, 0usize);
    let result = loop {
        if cancel.load(Ordering::Relaxed) {
            break Err("cancelled".into());
        }
        if let Some(sample) = sink.try_pull_sample(gst::ClockTime::from_mseconds(100)) {
            let Some(buffer) = sample.buffer() else { continue };
            let map = buffer.map_readable()?;
            for chunk in map.as_slice().chunks_exact(4) {
                let s = f32::from_le_bytes(chunk.try_into().unwrap());
                acc += s * s;
                count += 1;
                if count == BUCKET {
                    peaks.push((acc / BUCKET as f32).sqrt());
                    (acc, count) = (0.0, 0);
                }
            }
            continue;
        }
        if sink.is_eos() {
            break Ok(peaks);
        }
        if let Some(msg) = bus.pop_filtered(&[gst::MessageType::Error]) {
            if let gst::MessageView::Error(err) = msg.view() {
                break Err(err.error().to_string().into());
            }
        }
    };

    pipeline.set_state(gst::State::Null)?;
    result
}

#[derive(Default)]
struct State {
    peaks: Vec<f32>,
    max: f32,
    progress: f64,
    drag: Option<f64>,
}

/// Столбчатая волна с ползунком; клик и перетаскивание — перемотка.
pub struct Waveform {
    area: gtk::DrawingArea,
    state: Rc<RefCell<State>>,
}

impl Waveform {
    pub fn new() -> Self {
        let area = gtk::DrawingArea::builder()
            .hexpand(true)
            .vexpand(true)
            .cursor(&gtk::gdk::Cursor::from_name("pointer", None).unwrap())
            .build();
        let state = Rc::new(RefCell::new(State::default()));

        area.set_draw_func(glib::clone!(
            #[strong]
            state,
            move |area, cr, w, h| draw(area, cr, w as f64, h as f64, &state.borrow())
        ));

        adw::StyleManager::default().connect_accent_color_notify(glib::clone!(
            #[weak]
            area,
            move |_| area.queue_draw()
        ));

        Self { area, state }
    }

    pub fn widget(&self) -> &gtk::DrawingArea {
        &self.area
    }

    pub fn connect_seek(&self, on_seek: impl Fn(f64) + 'static) {
        let drag = gtk::GestureDrag::new();
        let fraction = |area: &gtk::DrawingArea, x: f64| (x / area.width().max(1) as f64).clamp(0.0, 1.0);

        drag.connect_drag_begin(glib::clone!(
            #[weak(rename_to = area)]
            self.area,
            #[strong(rename_to = state)]
            self.state,
            move |_, x, _| {
                state.borrow_mut().drag = Some(fraction(&area, x));
                area.queue_draw();
            }
        ));
        drag.connect_drag_update(glib::clone!(
            #[weak(rename_to = area)]
            self.area,
            #[strong(rename_to = state)]
            self.state,
            move |gesture, dx, _| {
                let (x, _) = gesture.start_point().unwrap_or_default();
                state.borrow_mut().drag = Some(fraction(&area, x + dx));
                area.queue_draw();
            }
        ));
        drag.connect_drag_end(glib::clone!(
            #[weak(rename_to = area)]
            self.area,
            #[strong(rename_to = state)]
            self.state,
            move |gesture, dx, _| {
                let (x, _) = gesture.start_point().unwrap_or_default();
                let f = fraction(&area, x + dx);
                {
                    let mut state = state.borrow_mut();
                    state.drag = None;
                    state.progress = f;
                }
                area.queue_draw();
                on_seek(f);
            }
        ));
        self.area.add_controller(drag);
    }

    pub fn set_peaks(&self, peaks: Vec<f32>) {
        let mut state = self.state.borrow_mut();
        state.max = peaks.iter().copied().fold(0.0, f32::max);
        state.peaks = peaks;
        drop(state);
        self.area.queue_draw();
    }

    pub fn set_progress(&self, progress: f64) {
        let mut state = self.state.borrow_mut();
        if (state.progress - progress).abs() > f64::EPSILON {
            state.progress = progress;
            drop(state);
            self.area.queue_draw();
        }
    }
}

fn draw(area: &gtk::DrawingArea, cr: &gtk::cairo::Context, w: f64, h: f64, state: &State) {
    let fg = area.color();
    let accent = adw::StyleManager::default().accent_color_rgba();
    let head_x = (state.drag.unwrap_or(state.progress) * w).round();

    let bars = ((w - BAR_WIDTH) / BAR_STEP).floor().max(0.0) as usize + 1;
    let len = state.peaks.len();
    let mid = h / 2.0;
    let max_height = h * 0.65;

    for i in 0..bars {
        let amp = if len == 0 || state.max <= 0.0 {
            0.0
        } else {
            let a = (i * len / bars).min(len - 1);
            let b = ((i + 1) * len / bars).clamp(a + 1, len);
            state.peaks[a..b].iter().copied().fold(0.0, f32::max) / state.max
        };

        let x = i as f64 * BAR_STEP;
        let bar_height = (amp as f64 * max_height).max(2.0);
        let color = if x + BAR_WIDTH / 2.0 < head_x { &accent } else { &fg };
        cr.set_source_rgba(
            color.red() as f64,
            color.green() as f64,
            color.blue() as f64,
            color.alpha() as f64 * 0.75,
        );
        cr.rectangle(x, mid - bar_height / 2.0, BAR_WIDTH, bar_height);
        let _ = cr.fill();
    }

    cr.set_source_rgba(
        accent.red() as f64,
        accent.green() as f64,
        accent.blue() as f64,
        1.0,
    );
    cr.rectangle((head_x - 1.0).max(0.0), 0.0, 2.0, h);
    let _ = cr.fill();
}
