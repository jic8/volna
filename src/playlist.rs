use std::cell::{Cell, RefCell};
use std::path::Path;
use std::rc::Rc;
use std::sync::mpsc;

use adw::prelude::*;
use gettextrs::{gettext, ngettext};
use gtk::{gio, glib};

const AUDIO_EXTENSIONS: &[&str] = &[
    "mp3", "flac", "ogg", "oga", "opus", "m4a", "aac", "wav", "wma", "ape", "wv", "mka", "aiff", "aif",
];
const CURRENT_MARKER: &str = "#PLAYER-CURRENT:";

pub enum Event {
    Activate(usize),
    Remove(usize),
}

#[derive(Clone)]
pub struct TrackInfo {
    pub id: u64,
    pub uri: String,
    pub title: String,
    pub artist: Option<String>,
    pub duration: Option<gst::ClockTime>,
}

struct Track {
    info: TrackInfo,
    row: gtk::ListBoxRow,
    title_label: gtk::Label,
    artist_label: gtk::Label,
    duration_label: gtk::Label,
    indicator: gtk::Image,
}

struct Meta {
    id: u64,
    title: Option<String>,
    artist: Option<String>,
    duration: Option<gst::ClockTime>,
}

/// Очередь треков и боковая панель для неё.
pub struct Playlist {
    widget: adw::ToolbarView,
    list: gtk::ListBox,
    summary: adw::WindowTitle,
    tracks: RefCell<Vec<Track>>,
    current: Cell<Option<usize>>,
    next_id: Cell<u64>,
    discover: mpsc::Sender<(u64, String)>,
    handler: RefCell<Option<Box<dyn Fn(Event)>>>,
}

impl Playlist {
    pub fn new() -> Rc<Self> {
        let summary = adw::WindowTitle::new(&gettext("Playlist"), "");

        let add_menu = gio::Menu::new();
        add_menu.append(Some(&gettext("Add Files…")), Some("win.add-files"));
        add_menu.append(Some(&gettext("Add Folder…")), Some("win.add-folder"));
        let add_button = gtk::MenuButton::builder()
            .icon_name("list-add-symbolic")
            .menu_model(&add_menu)
            .tooltip_text(gettext("Add"))
            .build();

        let menu = gio::Menu::new();
        menu.append(Some(&gettext("Open Playlist…")), Some("win.open"));
        menu.append(Some(&gettext("Save Playlist…")), Some("win.save-playlist"));
        menu.append(Some(&gettext("Clear")), Some("win.clear-playlist"));
        let menu_button = gtk::MenuButton::builder()
            .icon_name("view-more-symbolic")
            .menu_model(&menu)
            .tooltip_text(gettext("Playlist Menu"))
            .build();

        let header = adw::HeaderBar::builder().title_widget(&summary).build();
        header.pack_start(&add_button);
        header.pack_end(&menu_button);

        let list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .activate_on_single_click(false)
            .css_classes(["navigation-sidebar"])
            .build();
        list.set_placeholder(Some(
            &adw::StatusPage::builder()
                .icon_name("view-list-bullet-symbolic")
                .title(gettext("Playlist Is Empty"))
                .description(gettext("Add files or drop them into the window"))
                .css_classes(["compact"])
                .build(),
        ));
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&list)
            .build();

        let widget = adw::ToolbarView::new();
        widget.add_top_bar(&header);
        widget.set_content(Some(&scroller));

        let (discover, results) = spawn_discoverer();
        let this = Rc::new(Self {
            widget,
            list,
            summary,
            tracks: RefCell::new(Vec::new()),
            current: Cell::new(None),
            next_id: Cell::new(0),
            discover,
            handler: RefCell::new(None),
        });

        let weak = Rc::downgrade(&this);
        this.list.connect_row_activated(move |_, row| {
            if let (Some(this), Ok(i)) = (weak.upgrade(), usize::try_from(row.index())) {
                this.emit(Event::Activate(i));
            }
        });

        let weak = Rc::downgrade(&this);
        glib::spawn_future_local(async move {
            while let Ok(meta) = results.recv().await {
                let Some(this) = weak.upgrade() else { break };
                this.apply_meta(meta);
            }
        });

        this.update_summary();
        this
    }

    pub fn widget(&self) -> &adw::ToolbarView {
        &self.widget
    }

    pub fn connect_event(&self, f: impl Fn(Event) + 'static) {
        self.handler.replace(Some(Box::new(f)));
    }

    fn emit(&self, event: Event) {
        if let Some(handler) = self.handler.borrow().as_ref() {
            handler(event);
        }
    }

    pub fn len(&self) -> usize {
        self.tracks.borrow().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn get(&self, i: usize) -> Option<TrackInfo> {
        self.tracks.borrow().get(i).map(|t| t.info.clone())
    }

    pub fn current(&self) -> Option<usize> {
        self.current.get()
    }

    pub fn next_index(&self) -> Option<usize> {
        let next = self.current.get().map_or(0, |i| i + 1);
        (next < self.len()).then_some(next)
    }

    pub fn prev_index(&self) -> Option<usize> {
        self.current.get().and_then(|i| i.checked_sub(1))
    }

    pub fn set_current(&self, index: Option<usize>) {
        let tracks = self.tracks.borrow();
        if let Some(t) = self.current.get().and_then(|i| tracks.get(i)) {
            t.indicator.set_opacity(0.0);
            t.row.remove_css_class("current-track");
        }
        if let Some(t) = index.and_then(|i| tracks.get(i)) {
            t.indicator.set_opacity(1.0);
            t.row.add_css_class("current-track");
        }
        self.current.set(index);
    }

    /// Добавляет треки в конец, возвращает индекс первого добавленного.
    pub fn add(self: &Rc<Self>, uris: Vec<String>) -> usize {
        let first = self.len();
        for uri in uris {
            let id = self.next_id.get();
            self.next_id.set(id + 1);
            let track = self.build_track(id, uri);
            self.list.append(&track.row);
            let _ = self.discover.send((id, track.info.uri.clone()));
            self.tracks.borrow_mut().push(track);
        }
        self.update_summary();
        first
    }

    pub fn remove(&self, i: usize) {
        let mut tracks = self.tracks.borrow_mut();
        if i >= tracks.len() {
            return;
        }
        let track = tracks.remove(i);
        self.list.remove(&track.row);
        drop(tracks);
        self.current.set(match self.current.get() {
            Some(c) if c == i => None,
            Some(c) if c > i => Some(c - 1),
            c => c,
        });
        self.update_summary();
    }

    pub fn clear(&self) {
        self.list.remove_all();
        self.tracks.borrow_mut().clear();
        self.current.set(None);
        self.update_summary();
    }

    pub fn to_m3u(&self, with_current: bool) -> String {
        let mut out = String::from("#EXTM3U\n");
        if let (true, Some(c)) = (with_current, self.current.get()) {
            out.push_str(&format!("{CURRENT_MARKER}{c}\n"));
        }
        for t in self.tracks.borrow().iter() {
            let secs = t.info.duration.map_or(-1, |d| d.seconds() as i64);
            let name = match &t.info.artist {
                Some(artist) => format!("{artist} - {}", t.info.title),
                None => t.info.title.clone(),
            };
            out.push_str(&format!("#EXTINF:{secs},{name}\n"));
            let file = gio::File::for_uri(&t.info.uri);
            match file.path() {
                Some(path) => out.push_str(&path.to_string_lossy()),
                None => out.push_str(&t.info.uri),
            }
            out.push('\n');
        }
        out
    }

    fn build_track(self: &Rc<Self>, id: u64, uri: String) -> Track {
        let title = title_from_uri(&uri);

        let indicator = gtk::Image::from_icon_name("media-playback-start-symbolic");
        indicator.set_opacity(0.0);
        indicator.add_css_class("track-indicator");
        let title_label = gtk::Label::builder()
            .label(&title)
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["track-title"])
            .build();
        let artist_label = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .visible(false)
            .css_classes(["caption", "dim-label"])
            .build();
        let labels = gtk::Box::new(gtk::Orientation::Vertical, 2);
        labels.set_hexpand(true);
        labels.set_valign(gtk::Align::Center);
        labels.append(&title_label);
        labels.append(&artist_label);

        let duration_label = gtk::Label::builder()
            .css_classes(["caption", "numeric", "dim-label"])
            .build();
        let remove_button = gtk::Button::builder()
            .icon_name("window-close-symbolic")
            .tooltip_text(gettext("Remove From Playlist"))
            .valign(gtk::Align::Center)
            .css_classes(["flat", "circular", "remove-button"])
            .build();

        let content = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        content.append(&indicator);
        content.append(&labels);
        content.append(&duration_label);
        content.append(&remove_button);
        let row = gtk::ListBoxRow::builder().child(&content).build();

        let weak = Rc::downgrade(self);
        remove_button.connect_clicked(glib::clone!(
            #[weak]
            row,
            move |_| {
                if let (Some(this), Ok(i)) = (weak.upgrade(), usize::try_from(row.index())) {
                    this.emit(Event::Remove(i));
                }
            }
        ));

        Track {
            info: TrackInfo {
                id,
                uri,
                title,
                artist: None,
                duration: None,
            },
            row,
            title_label,
            artist_label,
            duration_label,
            indicator,
        }
    }

    fn apply_meta(&self, meta: Meta) {
        let mut tracks = self.tracks.borrow_mut();
        let Some(track) = tracks.iter_mut().find(|t| t.info.id == meta.id) else {
            return;
        };
        if let Some(title) = meta.title {
            track.title_label.set_label(&title);
            track.info.title = title;
        }
        if let Some(artist) = meta.artist {
            track.artist_label.set_label(&artist);
            track.artist_label.set_visible(true);
            track.info.artist = Some(artist);
        }
        if let Some(duration) = meta.duration {
            track.duration_label.set_label(&format_duration(duration));
            track.info.duration = Some(duration);
        }
        drop(tracks);
        self.update_summary();
    }

    fn update_summary(&self) {
        let tracks = self.tracks.borrow();
        if tracks.is_empty() {
            self.summary.set_subtitle("");
            return;
        }
        let total: gst::ClockTime = tracks.iter().filter_map(|t| t.info.duration).sum();
        let n = tracks.len();
        let count = ngettext("{} track", "{} tracks", n as u32).replace("{}", &n.to_string());
        self.summary
            .set_subtitle(&format!("{count} · {}", format_duration(total)));
    }
}

/// Раскрывает папки (рекурсивно) и M3U в список URI. Блокирующая — вызывать вне UI-потока.
/// Возвращает также индекс текущего трека, если он записан в плейлисте.
pub fn expand(uris: Vec<String>) -> (Vec<String>, Option<usize>) {
    let mut out = Vec::new();
    let mut current = None;
    for uri in uris {
        let file = gio::File::for_uri(&uri);
        match file.path() {
            Some(path) if path.is_dir() => collect_dir(&path, &mut out),
            Some(path) if is_playlist(&path) => {
                let (tracks, cur) = read_m3u(&path);
                current = current.or(cur.map(|c| c + out.len()));
                out.extend(tracks);
            }
            _ => out.push(uri),
        }
    }
    (out, current)
}

fn collect_dir(dir: &Path, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut paths: Vec<_> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
    paths.sort();
    for path in paths {
        if path.is_dir() {
            collect_dir(&path, out);
        } else if is_audio(&path) {
            out.push(gio::File::for_path(&path).uri().to_string());
        }
    }
}

fn has_extension(path: &Path, exts: &[&str]) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| exts.iter().any(|x| e.eq_ignore_ascii_case(x)))
}

fn is_audio(path: &Path) -> bool {
    has_extension(path, AUDIO_EXTENSIONS)
}

fn is_playlist(path: &Path) -> bool {
    has_extension(path, &["m3u", "m3u8"])
}

fn read_m3u(path: &Path) -> (Vec<String>, Option<usize>) {
    let Ok(content) = std::fs::read_to_string(path) else {
        return (Vec::new(), None);
    };
    let base = path.parent().unwrap_or(Path::new("/"));
    let mut uris = Vec::new();
    let mut current = None;
    for line in content.lines().map(str::trim) {
        if let Some(c) = line.strip_prefix(CURRENT_MARKER) {
            current = c.parse().ok();
        } else if line.is_empty() || line.starts_with('#') {
            continue;
        } else if line.contains("://") {
            uris.push(line.to_owned());
        } else {
            uris.push(gio::File::for_path(base.join(line)).uri().to_string());
        }
    }
    (uris, current)
}

fn spawn_discoverer() -> (mpsc::Sender<(u64, String)>, async_channel::Receiver<Meta>) {
    let (tx, rx) = mpsc::channel::<(u64, String)>();
    let (result_tx, result_rx) = async_channel::unbounded();
    std::thread::spawn(move || {
        let Ok(discoverer) = gst_pbutils::Discoverer::new(gst::ClockTime::from_seconds(5)) else {
            return;
        };
        for (id, uri) in rx {
            let Ok(info) = discoverer.discover_uri(&uri) else { continue };
            let tags = info.tags();
            let tag = |get: fn(&gst::TagListRef) -> Option<String>| tags.as_ref().and_then(|t| get(t));
            let meta = Meta {
                id,
                title: tag(|t| t.get::<gst::tags::Title>().map(|v| v.get().to_owned())),
                artist: tag(|t| t.get::<gst::tags::Artist>().map(|v| v.get().to_owned())),
                duration: info.duration(),
            };
            if result_tx.send_blocking(meta).is_err() {
                break;
            }
        }
    });
    (tx, result_rx)
}

fn title_from_uri(uri: &str) -> String {
    let file = gio::File::for_uri(uri);
    file.basename()
        .and_then(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
        .unwrap_or_else(|| uri.to_owned())
}

pub fn format_duration(t: gst::ClockTime) -> String {
    let s = t.seconds();
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{:02}:{:02}", s / 60, s % 60)
    }
}

