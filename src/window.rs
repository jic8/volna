use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use adw::prelude::*;
use gettextrs::gettext;
use gtk::{gdk, gio, glib};

use crate::mpris::{self, Mpris};
use crate::player::{self, Player};
use crate::playlist::{self, Playlist, Repeat, format_duration as format_time};
use crate::waveform::{self, Waveform};

const RATES: [f64; 6] = [0.5, 0.75, 1.0, 1.25, 1.5, 2.0];
const SKIP: gst::ClockTime = gst::ClockTime::from_seconds(10);
/// «Предыдущий» дальше этой позиции перематывает в начало текущего трека
const RESTART_THRESHOLD: gst::ClockTime = gst::ClockTime::from_seconds(3);

#[derive(Clone, Copy, PartialEq)]
enum OpenMode {
    /// Заменить плейлист и начать воспроизведение
    Replace,
    /// Добавить в конец; играть, только если ничего не выбрано
    Append,
    /// Восстановить прошлую сессию без автозапуска
    Restore,
}

pub struct Window {
    window: adw::ApplicationWindow,
    split: adw::OverlaySplitView,
    toasts: adw::ToastOverlay,
    stack: gtk::Stack,
    title: adw::WindowTitle,
    waveform: Waveform,
    position_label: gtk::Label,
    duration_label: gtk::Label,
    play_button: gtk::Button,
    rate_label: gtk::Label,
    repeat_button: gtk::Button,
    volume_button: gtk::MenuButton,
    volume_scale: gtk::Scale,
    player: Player,
    mpris: RefCell<Option<Mpris>>,
    playing: Cell<bool>,
    rate: Cell<f64>,
    known_duration: Cell<Option<gst::ClockTime>>,
    playlist: Rc<Playlist>,
    analysis: RefCell<Option<Arc<AtomicBool>>>,
}

impl Window {
    pub fn new(app: &adw::Application) -> Rc<Self> {
        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title(gettext("Volna"))
            .default_width(820)
            .default_height(460)
            .build();

        // Заголовок
        let title = adw::WindowTitle::new("", "");
        let menu = gio::Menu::new();
        let open_section = gio::Menu::new();
        open_section.append(Some(&gettext("Open Files…")), Some("win.open"));
        open_section.append(Some(&gettext("Open Folder…")), Some("win.open-folder"));
        menu.append_section(None, &open_section);
        let add_section = gio::Menu::new();
        add_section.append(Some(&gettext("Add Files…")), Some("win.add-files"));
        add_section.append(Some(&gettext("Add Folder…")), Some("win.add-folder"));
        menu.append_section(None, &add_section);
        let playlist_section = gio::Menu::new();
        playlist_section.append(Some(&gettext("Save Playlist…")), Some("win.save-playlist"));
        menu.append_section(None, &playlist_section);
        let about_section = gio::Menu::new();
        about_section.append(Some(&gettext("_About Volna")), Some("app.about"));
        menu.append_section(None, &about_section);
        let menu_button = gtk::MenuButton::builder()
            .icon_name("view-more-symbolic")
            .menu_model(&menu)
            .tooltip_text(gettext("Main Menu"))
            .build();
        let playlist_button = gtk::ToggleButton::builder()
            .icon_name("view-list-bullet-symbolic")
            .action_name("win.show-playlist")
            .tooltip_text(gettext("Playlist"))
            .build();
        let header = adw::HeaderBar::builder().title_widget(&title).build();
        header.pack_start(&menu_button);
        header.pack_end(&playlist_button);

        // Пустое состояние
        let empty_buttons = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .halign(gtk::Align::Center)
            .build();
        empty_buttons.append(
            &gtk::Button::builder()
                .label(gettext("Open Files"))
                .action_name("win.open")
                .css_classes(["pill", "suggested-action"])
                .build(),
        );
        empty_buttons.append(
            &gtk::Button::builder()
                .label(gettext("Open Folder"))
                .action_name("win.open-folder")
                .css_classes(["pill"])
                .build(),
        );
        let empty = adw::StatusPage::builder()
            .icon_name("audio-x-generic-symbolic")
            .title(gettext("No Tracks"))
            .description(gettext("Open files or a folder, or drop them into the window"))
            .child(&empty_buttons)
            .build();

        // Волна и время
        let waveform = Waveform::new();
        let position_label = time_label();
        let duration_label = time_label();
        let times = gtk::CenterBox::new();
        times.set_start_widget(Some(&position_label));
        times.set_end_widget(Some(&duration_label));

        // Скорость
        let rate_label = gtk::Label::new(Some(&format_rate(1.0)));
        let rate_content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        rate_content.append(&gtk::Image::from_icon_name("speedometer-symbolic"));
        rate_content.append(&rate_label);
        let rate_list = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let rate_popover = gtk::Popover::builder().child(&rate_list).build();
        let rate_button = gtk::MenuButton::builder()
            .child(&rate_content)
            .popover(&rate_popover)
            .tooltip_text(gettext("Playback Speed"))
            .css_classes(["flat"])
            .build();

        // Транспорт
        let prev_button = transport_button("media-skip-backward-symbolic", &gettext("Previous Track"), "win.previous");
        let back_button = transport_button("skip-backward-10-symbolic", &gettext("Back 10 Seconds"), "win.seek-backward");
        back_button.add_css_class("skip-button");
        let play_button = transport_button("media-playback-start-symbolic", &gettext("Play"), "win.toggle");
        play_button.add_css_class("play-button");
        let forward_button = transport_button("skip-forward-10-symbolic", &gettext("Forward 10 Seconds"), "win.seek-forward");
        forward_button.add_css_class("skip-button");
        let next_button = transport_button("media-skip-forward-symbolic", &gettext("Next Track"), "win.next");
        let transport = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        transport.set_valign(gtk::Align::Center);
        for b in [&prev_button, &back_button, &play_button, &forward_button, &next_button] {
            transport.append(b);
        }

        // Повтор
        let repeat_button = gtk::Button::builder()
            .action_name("win.cycle-repeat")
            .css_classes(["flat"])
            .build();

        // Громкость
        let volume_scale = gtk::Scale::with_range(gtk::Orientation::Vertical, 0.0, 1.0, 0.01);
        volume_scale.set_inverted(true);
        volume_scale.set_height_request(140);
        volume_scale.set_value(1.0);
        let volume_button = gtk::MenuButton::builder()
            .icon_name("audio-volume-high-symbolic")
            .popover(&gtk::Popover::builder().child(&volume_scale).build())
            .tooltip_text(gettext("Volume"))
            .css_classes(["flat"])
            .build();

        let end_buttons = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        end_buttons.append(&repeat_button);
        end_buttons.append(&volume_button);

        let controls = gtk::CenterBox::new();
        controls.set_start_widget(Some(&rate_button));
        controls.set_center_widget(Some(&transport));
        controls.set_end_widget(Some(&end_buttons));
        for w in [rate_button.upcast_ref::<gtk::Widget>(), end_buttons.upcast_ref()] {
            w.set_valign(gtk::Align::Center);
        }

        let page = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .margin_start(18)
            .margin_end(18)
            .margin_top(6)
            .margin_bottom(18)
            .build();
        page.append(waveform.widget());
        page.append(&times);
        page.append(&controls);

        let stack = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .build();
        stack.add_named(&empty, Some("empty"));
        stack.add_named(&page, Some("player"));

        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&stack));
        let view = adw::ToolbarView::new();
        view.add_top_bar(&header);
        view.set_content(Some(&toasts));

        // Плейлист сбоку; в узком окне выезжает поверх
        let playlist = Playlist::new();
        let split = adw::OverlaySplitView::builder()
            .sidebar_position(gtk::PackType::End)
            .content(&view)
            .sidebar(playlist.widget())
            .show_sidebar(false)
            .min_sidebar_width(240.0)
            .max_sidebar_width(320.0)
            .build();
        window.set_content(Some(&split));
        window.add_action(&gio::PropertyAction::new("show-playlist", &split, "show-sidebar"));

        let breakpoint = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
            adw::BreakpointConditionLengthType::MaxWidth,
            720.0,
            adw::LengthUnit::Sp,
        ));
        breakpoint.add_setter(&split, "collapsed", Some(&true.to_value()));
        window.add_breakpoint(breakpoint);

        let this = Rc::new(Self {
            window,
            split,
            toasts,
            stack,
            title,
            waveform,
            position_label,
            duration_label,
            play_button,
            rate_label,
            repeat_button,
            volume_button,
            volume_scale: volume_scale.clone(),
            player: Player::new(),
            mpris: RefCell::new(None),
            playing: Cell::new(false),
            rate: Cell::new(1.0),
            known_duration: Cell::new(None),
            playlist,
            analysis: RefCell::new(None),
        });

        this.setup_actions();
        this.setup_rates(&rate_list, &rate_popover);
        this.setup_signals(&volume_scale);
        this.setup_mpris(app);
        this.set_repeat(Repeat::Off);
        this
    }

    fn setup_mpris(self: &Rc<Self>, app: &adw::Application) {
        let Some(connection) = app.dbus_connection() else { return };
        let backend: std::rc::Weak<dyn mpris::Backend> = Rc::downgrade(self) as _;
        match Mpris::new(connection, backend) {
            Ok(mpris) => {
                self.mpris.replace(Some(mpris));
            }
            Err(err) => eprintln!("MPRIS unavailable: {err}"),
        }
    }

    fn notify_mpris(&self, properties: &[&str]) {
        if let Some(mpris) = self.mpris.borrow().as_ref() {
            mpris.notify(properties);
        }
    }

    fn seek_to(&self, pos: gst::ClockTime) {
        self.player.seek(pos);
        self.seeked(pos);
    }

    fn seeked(&self, pos: gst::ClockTime) {
        if let Some(mpris) = self.mpris.borrow().as_ref() {
            mpris.seeked(pos);
        }
    }

    fn set_rate(&self, rate: f64) {
        self.rate.set(rate);
        self.player.set_rate(rate);
        self.rate_label.set_label(&format_rate(rate));
        self.notify_mpris(&["Rate"]);
    }

    fn set_repeat(&self, repeat: Repeat) {
        self.playlist.set_repeat(repeat);
        let (icon, tooltip) = match repeat {
            Repeat::Off => ("playlist-repeat-symbolic", gettext("Repeat Off")),
            Repeat::Playlist => ("playlist-repeat-symbolic", gettext("Repeat Playlist")),
            Repeat::Track => ("playlist-repeat-track-symbolic", gettext("Repeat Track")),
        };
        self.repeat_button.set_icon_name(icon);
        self.repeat_button.set_tooltip_text(Some(&tooltip));
        if repeat == Repeat::Off {
            self.repeat_button.remove_css_class("accent");
            self.repeat_button.add_css_class("dim-label");
        } else {
            self.repeat_button.remove_css_class("dim-label");
            self.repeat_button.add_css_class("accent");
        }
        self.update_actions();
        self.notify_mpris(&["LoopStatus"]);
    }

    fn cycle_repeat(&self) {
        self.set_repeat(match self.playlist.repeat() {
            Repeat::Off => Repeat::Playlist,
            Repeat::Playlist => Repeat::Track,
            Repeat::Track => Repeat::Off,
        });
    }

    pub fn present(&self) {
        self.window.present();
    }

    pub fn open_files(self: &Rc<Self>, files: &[gio::File]) {
        self.open_uris(files.iter().map(|f| f.uri().into()).collect(), OpenMode::Replace);
    }

    pub fn restore_session(self: &Rc<Self>) {
        let path = session_path();
        if path.exists() {
            let uri = gio::File::for_path(&path).uri().into();
            self.open_uris(vec![uri], OpenMode::Restore);
        }
    }

    fn save_session(&self) {
        let path = session_path();
        let result = if self.playlist.is_empty() {
            std::fs::remove_file(&path).or_else(|e| match e.kind() {
                std::io::ErrorKind::NotFound => Ok(()),
                _ => Err(e),
            })
        } else {
            std::fs::create_dir_all(path.parent().unwrap())
                .and_then(|_| std::fs::write(&path, self.playlist.to_m3u(true)))
        };
        if let Err(err) = result {
            eprintln!("failed to save session: {err}");
        }
    }

    /// Папки и M3U раскрываются в фоне, затем треки попадают в плейлист.
    fn open_uris(self: &Rc<Self>, uris: Vec<String>, mode: OpenMode) {
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Ok((uris, current)) = gio::spawn_blocking(move || playlist::expand(uris)).await else {
                return;
            };
            let Some(this) = weak.upgrade() else { return };
            if uris.is_empty() {
                if mode != OpenMode::Restore {
                    this.toasts.add_toast(adw::Toast::new(&gettext("No audio files found")));
                }
                return;
            }

            if mode != OpenMode::Append {
                this.clear_playlist();
            }
            let first = this.playlist.add(uris);
            // В узком окне панель перекрыла бы плеер — там открывают вручную
            if mode != OpenMode::Restore && this.playlist.len() > 1 && !this.split.is_collapsed() {
                this.split.set_show_sidebar(true);
            }
            let start = (first + current.unwrap_or(0)).min(this.playlist.len() - 1);
            match mode {
                OpenMode::Replace => this.play_index(start, true),
                OpenMode::Restore => this.play_index(start, false),
                OpenMode::Append if this.playlist.current().is_none() => this.play_index(first, true),
                OpenMode::Append => {}
            }
            this.update_actions();
        });
    }

    fn play_index(self: &Rc<Self>, index: usize, autoplay: bool) {
        let Some(track) = self.playlist.get(index) else { return };
        self.playlist.set_current(Some(index));
        self.title.set_title(&track.title);
        self.title.set_subtitle(track.artist.as_deref().unwrap_or(""));
        self.stack.set_visible_child_name("player");
        self.waveform.set_peaks(Vec::new());
        self.waveform.set_progress(0.0);
        self.position_label.set_label(&format_time(gst::ClockTime::ZERO));
        self.duration_label.set_label(&format_time(gst::ClockTime::ZERO));
        self.known_duration.set(None);
        self.player.load(&track.uri, autoplay);
        self.update_actions();

        let cancel = Arc::new(AtomicBool::new(false));
        if let Some(old) = self.analysis.replace(Some(cancel.clone())) {
            old.store(true, Ordering::Relaxed);
        }
        let rx = waveform::analyze(track.uri, cancel.clone());
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            if let (Ok(peaks), Some(this)) = (rx.recv().await, weak.upgrade()) {
                if !cancel.load(Ordering::Relaxed) {
                    this.waveform.set_peaks(peaks);
                }
            }
        });
    }

    fn clear_playlist(&self) {
        self.player.stop();
        self.playlist.clear();
        if let Some(old) = self.analysis.take() {
            old.store(true, Ordering::Relaxed);
        }
        self.title.set_title("");
        self.title.set_subtitle("");
        self.play_button.set_icon_name("media-playback-start-symbolic");
        self.playing.set(false);
        self.known_duration.set(None);
        self.stack.set_visible_child_name("empty");
        self.update_actions();
    }

    fn toggle(self: &Rc<Self>) {
        match self.playlist.current() {
            Some(_) => self.player.toggle(),
            None if !self.playlist.is_empty() => self.play_index(0, true),
            None => {}
        }
    }

    fn skip(&self, forward: bool) {
        if let Some(pos) = self.player.skip(SKIP, forward) {
            self.seeked(pos);
        }
    }

    fn next(self: &Rc<Self>) {
        if let Some(i) = self.playlist.next_index() {
            self.play_index(i, true);
        }
    }

    fn previous(self: &Rc<Self>) {
        let pos = self.player.position().unwrap_or_default();
        match self.playlist.prev_index() {
            Some(i) if pos < RESTART_THRESHOLD => self.play_index(i, true),
            _ => self.seek_to(gst::ClockTime::ZERO),
        }
    }

    fn update_actions(&self) {
        let has_current = self.playlist.current().is_some();
        let has_tracks = !self.playlist.is_empty();
        for (name, enabled) in [
            ("previous", has_current),
            ("next", has_current && self.playlist.next_index().is_some()),
            ("toggle", has_tracks),
            ("seek-backward", has_current),
            ("seek-forward", has_current),
            ("save-playlist", has_tracks),
            ("clear-playlist", has_tracks),
        ] {
            if let Some(action) = self.window.lookup_action(name).and_downcast::<gio::SimpleAction>() {
                action.set_enabled(enabled);
            }
        }
        self.notify_mpris(&[
            "PlaybackStatus",
            "Metadata",
            "CanGoNext",
            "CanGoPrevious",
            "CanPlay",
            "CanPause",
            "CanSeek",
        ]);
    }

    fn file_dialog(title: &str) -> gtk::FileDialog {
        let filter = gtk::FileFilter::new();
        filter.set_name(Some(&gettext("Audio and Playlists")));
        filter.add_mime_type("audio/*");
        filter.add_suffix("m3u");
        filter.add_suffix("m3u8");
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&filter);
        gtk::FileDialog::builder()
            .title(title)
            .filters(&filters)
            .modal(true)
            .build()
    }

    fn choose_files(self: &Rc<Self>, mode: OpenMode) {
        let dialog = Self::file_dialog(&if mode == OpenMode::Append {
            gettext("Add Files")
        } else {
            gettext("Open Files")
        });
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Some(this) = weak.upgrade() else { return };
            let Ok(files) = dialog.open_multiple_future(Some(&this.window)).await else {
                return;
            };
            let uris = files
                .iter::<gio::File>()
                .filter_map(Result::ok)
                .map(|f| f.uri().into())
                .collect();
            this.open_uris(uris, mode);
        });
    }

    fn choose_folder(self: &Rc<Self>, mode: OpenMode) {
        let dialog = gtk::FileDialog::builder()
            .title(gettext("Choose a Music Folder"))
            .modal(true)
            .build();
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Some(this) = weak.upgrade() else { return };
            if let Ok(folder) = dialog.select_folder_future(Some(&this.window)).await {
                this.open_uris(vec![folder.uri().into()], mode);
            }
        });
    }

    fn save_playlist(self: &Rc<Self>) {
        let dialog = gtk::FileDialog::builder()
            .title(gettext("Save Playlist"))
            .initial_name(gettext("Playlist.m3u"))
            .modal(true)
            .build();
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Some(this) = weak.upgrade() else { return };
            let Ok(file) = dialog.save_future(Some(&this.window)).await else {
                return;
            };
            let message = match file.path().map(|p| std::fs::write(p, this.playlist.to_m3u(false))) {
                Some(Ok(())) => gettext("Playlist saved"),
                Some(Err(err)) => gettext("Could not save playlist: {}").replace("{}", &err.to_string()),
                None => gettext("Playlists can only be saved to a local folder"),
            };
            this.toasts.add_toast(adw::Toast::new(&message));
        });
    }

    fn setup_actions(self: &Rc<Self>) {
        let action = |name: &str, f: fn(&Rc<Self>)| {
            let weak = Rc::downgrade(self);
            gio::ActionEntry::builder(name)
                .activate(move |_: &adw::ApplicationWindow, _, _| {
                    if let Some(this) = weak.upgrade() {
                        f(&this);
                    }
                })
                .build()
        };
        self.window.add_action_entries([
            action("open", |this| this.choose_files(OpenMode::Replace)),
            action("open-folder", |this| this.choose_folder(OpenMode::Replace)),
            action("add-files", |this| this.choose_files(OpenMode::Append)),
            action("add-folder", |this| this.choose_folder(OpenMode::Append)),
            action("save-playlist", |this| this.save_playlist()),
            action("clear-playlist", |this| this.clear_playlist()),
            action("toggle", |this| this.toggle()),
            action("next", |this| this.next()),
            action("previous", |this| this.previous()),
            action("seek-backward", |this| this.skip(false)),
            action("seek-forward", |this| this.skip(true)),
            action("cycle-repeat", |this| this.cycle_repeat()),
        ]);
    }

    fn setup_rates(self: &Rc<Self>, list: &gtk::Box, popover: &gtk::Popover) {
        for rate in RATES {
            let button = gtk::Button::builder()
                .label(format_rate(rate))
                .css_classes(["flat"])
                .build();
            let weak = Rc::downgrade(self);
            button.connect_clicked(glib::clone!(
                #[weak]
                popover,
                move |_| {
                    if let Some(this) = weak.upgrade() {
                        this.set_rate(rate);
                    }
                    popover.popdown();
                }
            ));
            list.append(&button);
        }
    }

    fn setup_signals(self: &Rc<Self>, volume_scale: &gtk::Scale) {
        let weak = Rc::downgrade(self);
        self.player.connect_event(move |event| {
            let Some(this) = weak.upgrade() else { return };
            this.handle_player_event(event);
        });

        let weak = Rc::downgrade(self);
        self.playlist.connect_event(move |event| {
            let Some(this) = weak.upgrade() else { return };
            this.handle_playlist_event(event);
        });

        let weak = Rc::downgrade(self);
        self.waveform.connect_seek(move |fraction| {
            let Some(this) = weak.upgrade() else { return };
            if let Some(dur) = this.player.duration() {
                let pos = gst::ClockTime::from_nseconds((dur.nseconds() as f64 * fraction) as u64);
                this.seek_to(pos);
            }
        });

        let weak = Rc::downgrade(self);
        volume_scale.connect_value_changed(move |scale| {
            let Some(this) = weak.upgrade() else { return };
            let v = scale.value();
            this.player.set_volume(v);
            this.volume_button.set_icon_name(match v {
                v if v <= 0.0 => "audio-volume-muted-symbolic",
                v if v < 0.34 => "audio-volume-low-symbolic",
                v if v < 0.67 => "audio-volume-medium-symbolic",
                _ => "audio-volume-high-symbolic",
            });
            this.notify_mpris(&["Volume"]);
        });

        let drop = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
        let weak = Rc::downgrade(self);
        drop.connect_drop(move |_, value, _, _| {
            let (Some(this), Ok(list)) = (weak.upgrade(), value.get::<gdk::FileList>()) else {
                return false;
            };
            let uris = list.files().iter().map(|f| f.uri().into()).collect();
            this.open_uris(uris, OpenMode::Append);
            true
        });
        self.window.add_controller(drop);

        let weak = Rc::downgrade(self);
        glib::timeout_add_local(Duration::from_millis(33), move || match weak.upgrade() {
            Some(this) => {
                this.tick();
                glib::ControlFlow::Continue
            }
            None => glib::ControlFlow::Break,
        });

        // Сильная ссылка: обработчик живёт ровно столько, сколько окно,
        // и держит состояние (включая плеер) до закрытия.
        let this = self.clone();
        self.window.connect_close_request(move |_| {
            this.save_session();
            this.player.stop();
            glib::Propagation::Proceed
        });
    }

    fn handle_player_event(self: &Rc<Self>, event: player::Event) {
        use player::Event;

        match event {
            Event::Playing(playing) => {
                let (icon, tooltip) = if playing {
                    ("media-playback-pause-symbolic", gettext("Pause"))
                } else {
                    ("media-playback-start-symbolic", gettext("Play"))
                };
                self.play_button.set_icon_name(icon);
                self.play_button.set_tooltip_text(Some(&tooltip));
                self.playing.set(playing);
                self.notify_mpris(&["PlaybackStatus"]);
            }
            Event::Eos if self.playlist.repeat() == Repeat::Track => {
                self.seek_to(gst::ClockTime::ZERO);
            }
            Event::Eos => match self.playlist.next_index() {
                Some(i) => self.play_index(i, true),
                None => {
                    self.player.rewind();
                    self.seeked(gst::ClockTime::ZERO);
                }
            },
            Event::Tags { title, artist } => {
                if let Some(title) = title {
                    self.title.set_title(&title);
                }
                if let Some(artist) = artist {
                    self.title.set_subtitle(&artist);
                }
            }
            Event::Error(msg) => self.toasts.add_toast(adw::Toast::new(&msg)),
        }
    }

    fn handle_playlist_event(self: &Rc<Self>, event: playlist::Event) {
        use playlist::Event;

        match event {
            Event::Activate(i) => self.play_index(i, true),
            Event::Remove(i) => self.remove_track(i),
            Event::Trash(i) => self.trash_track(i),
        }
    }

    fn remove_track(self: &Rc<Self>, i: usize) {
        let was_current = self.playlist.current() == Some(i);
        let was_playing = self.player.is_playing();
        self.playlist.remove(i);
        if was_current {
            match self.playlist.len() {
                0 => self.clear_playlist(),
                len => self.play_index(i.min(len - 1), was_playing),
            }
        }
        self.update_actions();
    }

    /// Отправляет файл трека в корзину; если корзины нет — предлагает удалить насовсем.
    fn trash_track(self: &Rc<Self>, i: usize) {
        let Some(track) = self.playlist.get(i) else { return };
        let file = gio::File::for_uri(&track.uri);
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let result = file.trash_future(glib::Priority::DEFAULT).await;
            let Some(this) = weak.upgrade() else { return };
            let result = match result {
                Err(err) if err.matches(gio::IOErrorEnum::NotSupported) => {
                    if !this.confirm_delete(&file).await {
                        return;
                    }
                    file.delete_future(glib::Priority::DEFAULT).await.map(|()| gettext("File deleted"))
                }
                result => result.map(|()| gettext("File moved to trash")),
            };
            let message = match result {
                Ok(message) => {
                    if let Some(i) = this.playlist.index_of(track.id) {
                        this.remove_track(i);
                    }
                    message
                }
                Err(err) => gettext("Could not delete file: {}").replace("{}", err.message()),
            };
            this.toasts.add_toast(adw::Toast::new(&message));
        });
    }

    async fn confirm_delete(&self, file: &gio::File) -> bool {
        let name = file
            .basename()
            .map_or_else(|| file.uri().to_string(), |p| p.to_string_lossy().into_owned());
        let dialog = adw::AlertDialog::new(
            Some(&gettext("Delete File Permanently?")),
            Some(&gettext("Trash is not available here. “{}” will be deleted permanently.").replace("{}", &name)),
        );
        dialog.add_responses(&[("cancel", &gettext("Cancel")), ("delete", &gettext("Delete"))]);
        dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        dialog.choose_future(Some(&self.window)).await == "delete"
    }

    fn tick(&self) {
        let Some(dur) = self.player.duration() else { return };
        if self.known_duration.replace(Some(dur)) != Some(dur) {
            self.notify_mpris(&["Metadata"]);
        }
        let pos = self.player.position().unwrap_or_default().min(dur);
        self.position_label.set_label(&format_time(pos));
        self.duration_label.set_label(&format_time(dur));
        if dur > gst::ClockTime::ZERO {
            self.waveform
                .set_progress(pos.nseconds() as f64 / dur.nseconds() as f64);
        }
    }
}

impl mpris::Backend for Window {
    fn status(&self) -> mpris::Status {
        match (self.playlist.current(), self.playing.get()) {
            (None, _) => mpris::Status::Stopped,
            (Some(_), true) => mpris::Status::Playing,
            (Some(_), false) => mpris::Status::Paused,
        }
    }

    fn track(&self) -> Option<mpris::Track> {
        let info = self.playlist.current().and_then(|i| self.playlist.get(i))?;
        Some(mpris::Track {
            id: info.id,
            title: info.title,
            artist: info.artist,
            uri: info.uri,
            length: self.known_duration.get().or(info.duration),
        })
    }

    fn position(&self) -> gst::ClockTime {
        self.player.position().unwrap_or_default()
    }

    fn volume(&self) -> f64 {
        self.volume_scale.value()
    }

    fn rate(&self) -> f64 {
        self.rate.get()
    }

    fn repeat(&self) -> Repeat {
        self.playlist.repeat()
    }

    fn can_go_next(&self) -> bool {
        self.playlist.current().is_some() && self.playlist.next_index().is_some()
    }

    fn can_go_previous(&self) -> bool {
        self.playlist.current().is_some()
    }

    fn command(self: Rc<Self>, command: mpris::Command) {
        use mpris::Command;

        match command {
            Command::Raise => self.window.present(),
            Command::Quit => self.window.close(),
            Command::PlayPause => self.toggle(),
            Command::Play if !self.playing.get() => self.toggle(),
            Command::Pause if self.playing.get() => self.player.toggle(),
            Command::Play | Command::Pause => {}
            Command::Stop => {
                self.player.rewind();
                self.seeked(gst::ClockTime::ZERO);
            }
            Command::Next => self.next(),
            Command::Previous => self.previous(),
            Command::Seek(offset) => {
                let (Some(pos), Some(dur)) = (self.player.position(), self.player.duration()) else {
                    return;
                };
                let target = pos.nseconds() as i64 / 1000 + offset;
                if target > (dur.nseconds() / 1000) as i64 {
                    self.next();
                } else {
                    self.seek_to(gst::ClockTime::from_useconds(target.max(0) as u64));
                }
            }
            Command::SetPosition(us) => {
                let Some(dur) = self.player.duration() else { return };
                let pos = gst::ClockTime::from_useconds(us.max(0) as u64);
                if pos <= dur {
                    self.seek_to(pos);
                }
            }
            Command::OpenUri(uri) => self.open_uris(vec![uri], OpenMode::Replace),
            Command::SetVolume(v) => self.volume_scale.set_value(v),
            Command::SetRate(rate) => self.set_rate(rate),
            Command::SetRepeat(repeat) => self.set_repeat(repeat),
        }
    }
}

fn session_path() -> PathBuf {
    glib::user_data_dir().join("volna").join("session.m3u")
}

fn time_label() -> gtk::Label {
    gtk::Label::builder()
        .label(format_time(gst::ClockTime::ZERO))
        .css_classes(["caption", "numeric", "dim-label"])
        .build()
}

fn transport_button(icon: &str, tooltip: &str, action: &str) -> gtk::Button {
    gtk::Button::builder()
        .icon_name(icon)
        .tooltip_text(tooltip)
        .action_name(action)
        .focus_on_click(false)
        .css_classes(["flat", "circular"])
        .valign(gtk::Align::Center)
        .build()
}

fn format_rate(rate: f64) -> String {
    if rate.fract() == 0.0 {
        format!("× {rate:.1}")
    } else {
        format!("× {rate}")
    }
}
