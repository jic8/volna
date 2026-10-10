use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gst::glib;
use gst::prelude::*;

pub enum Event {
    Playing(bool),
    Eos,
    Tags {
        title: Option<String>,
        artist: Option<String>,
    },
    Error(String),
}

/// Обёртка над playbin: воспроизведение, перемотка, скорость с сохранением тона.
pub struct Player {
    inner: Rc<Inner>,
    _watch: gst::bus::BusWatchGuard,
}

struct Inner {
    playbin: gst::Element,
    rate: Cell<f64>,
    rate_pending: Cell<bool>,
    handler: RefCell<Option<Box<dyn Fn(Event)>>>,
}

impl Player {
    pub fn new() -> Self {
        let playbin = gst::ElementFactory::make("playbin")
            .property("video-sink", gst::ElementFactory::make("fakesink").build().unwrap())
            .build()
            .expect("playbin element is missing");
        // scaletempo сохраняет высоту тона при изменении скорости
        if let Ok(scaletempo) = gst::ElementFactory::make("scaletempo").build() {
            playbin.set_property("audio-filter", scaletempo);
        }

        let inner = Rc::new(Inner {
            playbin,
            rate: Cell::new(1.0),
            rate_pending: Cell::new(false),
            handler: RefCell::new(None),
        });

        let weak = Rc::downgrade(&inner);
        let watch = inner
            .playbin
            .bus()
            .unwrap()
            .add_watch_local(move |_, msg| {
                if let Some(inner) = weak.upgrade() {
                    inner.handle_message(msg);
                }
                glib::ControlFlow::Continue
            })
            .expect("failed to watch the bus");

        Self { inner, _watch: watch }
    }

    pub fn connect_event(&self, f: impl Fn(Event) + 'static) {
        self.inner.handler.replace(Some(Box::new(f)));
    }

    pub fn load(&self, uri: &str, autoplay: bool) {
        let playbin = &self.inner.playbin;
        let _ = playbin.set_state(gst::State::Null);
        playbin.set_property("uri", uri);
        self.inner.rate_pending.set(self.inner.rate.get() != 1.0);
        let state = if autoplay {
            gst::State::Playing
        } else {
            gst::State::Paused
        };
        let _ = playbin.set_state(state);
    }

    /// Пауза и возврат в начало трека.
    pub fn rewind(&self) {
        let _ = self.inner.playbin.set_state(gst::State::Paused);
        self.seek(gst::ClockTime::ZERO);
    }

    pub fn stop(&self) {
        let _ = self.inner.playbin.set_state(gst::State::Null);
    }

    pub fn toggle(&self) {
        let state = if self.is_playing() {
            gst::State::Paused
        } else {
            gst::State::Playing
        };
        let _ = self.inner.playbin.set_state(state);
    }

    pub fn is_playing(&self) -> bool {
        let (_, current, pending) = self.inner.playbin.state(gst::ClockTime::ZERO);
        let target = if pending == gst::State::VoidPending {
            current
        } else {
            pending
        };
        target == gst::State::Playing
    }

    pub fn position(&self) -> Option<gst::ClockTime> {
        self.inner.playbin.query_position()
    }

    pub fn duration(&self) -> Option<gst::ClockTime> {
        self.inner.playbin.query_duration()
    }

    pub fn seek(&self, pos: gst::ClockTime) {
        self.inner.seek(pos);
    }

    /// Перематывает на `offset` и возвращает новую позицию.
    pub fn skip(&self, offset: gst::ClockTime, forward: bool) -> Option<gst::ClockTime> {
        let (Some(pos), Some(dur)) = (self.position(), self.duration()) else {
            return None;
        };
        let target = if forward {
            (pos + offset).min(dur)
        } else {
            pos.saturating_sub(offset)
        };
        self.seek(target);
        Some(target)
    }

    pub fn set_rate(&self, rate: f64) {
        self.inner.rate.set(rate);
        if let Some(pos) = self.position() {
            self.seek(pos);
        }
    }

    /// `volume` в диапазоне 0..1, шкала кубическая — так громкость воспринимается линейно.
    pub fn set_volume(&self, volume: f64) {
        self.inner.playbin.set_property("volume", volume.powi(3));
    }
}

impl Inner {
    fn seek(&self, pos: gst::ClockTime) {
        let _ = self.playbin.seek(
            self.rate.get(),
            gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE,
            gst::SeekType::Set,
            Some(pos),
            gst::SeekType::None,
            gst::ClockTime::NONE,
        );
    }

    fn emit(&self, event: Event) {
        if let Some(handler) = self.handler.borrow().as_ref() {
            handler(event);
        }
    }

    fn handle_message(self: &Rc<Self>, msg: &gst::Message) {
        use gst::MessageView;

        match msg.view() {
            MessageView::Eos(_) => self.emit(Event::Eos),
            MessageView::Error(err) => {
                let _ = self.playbin.set_state(gst::State::Null);
                self.emit(Event::Error(err.error().to_string()));
            }
            MessageView::Tag(tag) => {
                let tags = tag.tags();
                let title = tags.get::<gst::tags::Title>().map(|v| v.get().to_owned());
                let artist = tags.get::<gst::tags::Artist>().map(|v| v.get().to_owned());
                if title.is_some() || artist.is_some() {
                    self.emit(Event::Tags { title, artist });
                }
            }
            MessageView::AsyncDone(_) => {
                // скорость сбрасывается при загрузке нового потока — применяем после preroll
                if self.rate_pending.replace(false) {
                    if let Some(pos) = self.playbin.query_position() {
                        self.seek(pos);
                    }
                }
            }
            MessageView::StateChanged(change) if self.is_source(msg) => {
                self.emit(Event::Playing(change.current() == gst::State::Playing));
            }
            _ => {}
        }
    }

    fn is_source(&self, msg: &gst::Message) -> bool {
        msg.src() == Some(self.playbin.upcast_ref())
    }
}

