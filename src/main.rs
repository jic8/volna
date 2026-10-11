mod mpris;
mod player;
mod playlist;
mod waveform;
mod window;

use std::cell::RefCell;
use std::rc::{Rc, Weak};

use adw::prelude::*;
use gettextrs::{LocaleCategory, gettext};
use gtk::{gdk, gio, glib};

const APP_ID: &str = "io.github.jic8.Volna";
const GETTEXT_PACKAGE: &str = "volna";
const LOCALEDIR: &str = env!("VOLNA_LOCALEDIR");

fn main() -> glib::ExitCode {
    // SAFETY: вызывается первым делом в main, до запуска других потоков
    unsafe {
        gettextrs::setlocale(LocaleCategory::LcAll, "");
    }
    let _ = gettextrs::bindtextdomain(GETTEXT_PACKAGE, LOCALEDIR);
    let _ = gettextrs::bind_textdomain_codeset(GETTEXT_PACKAGE, "UTF-8");
    let _ = gettextrs::textdomain(GETTEXT_PACKAGE);
    glib::set_application_name(&gettext("Volna"));

    gst::init().expect("failed to initialize GStreamer");
    gio::resources_register_include!("resources.gresource").expect("failed to register resources");

    let app = adw::Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::HANDLES_OPEN)
        .build();

    app.connect_startup(|app| {
        let css = gtk::CssProvider::new();
        css.load_from_string(include_str!("style.css"));
        gtk::style_context_add_provider_for_display(
            &gdk::Display::default().expect("no display"),
            &css,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );

        app.add_action_entries([gio::ActionEntry::builder("about")
            .activate(|app: &adw::Application, _, _| show_about(app))
            .build()]);

        app.set_accels_for_action("win.open", &["<Control>o"]);
        app.set_accels_for_action("win.toggle", &["space"]);
        app.set_accels_for_action("win.seek-backward", &["Left"]);
        app.set_accels_for_action("win.seek-forward", &["Right"]);
        app.set_accels_for_action("win.previous", &["<Control>Left"]);
        app.set_accels_for_action("win.next", &["<Control>Right"]);
        app.set_accels_for_action("win.add-files", &["<Control><Shift>o"]);
        app.set_accels_for_action("win.show-playlist", &["F9"]);
        app.set_accels_for_action("win.cycle-repeat", &["<Control>r"]);
        app.set_accels_for_action("window.close", &["<Control>w"]);
    });

    app.connect_activate(|app| {
        let (win, created) = main_window(app);
        if created {
            win.restore_session();
        }
        win.present();
    });

    // Повторный запуск с файлами открывает их в уже работающем окне
    app.connect_open(|app, files, _| {
        let (win, _) = main_window(app);
        win.open_files(files);
        win.present();
    });

    app.run()
}

thread_local! {
    static MAIN_WINDOW: RefCell<Weak<window::Window>> = RefCell::new(Weak::new());
}

/// Окно у приложения одно: MPRIS и сессия рассчитаны на один плеер.
fn main_window(app: &adw::Application) -> (Rc<window::Window>, bool) {
    MAIN_WINDOW.with(|cell| {
        if let Some(win) = cell.borrow().upgrade() {
            return (win, false);
        }
        let win = window::Window::new(app);
        cell.replace(Rc::downgrade(&win));
        (win, true)
    })
}

fn show_about(app: &adw::Application) {
    let about = adw::AboutDialog::from_appdata(
        "/io/github/jic8/Volna/io.github.jic8.Volna.metainfo.xml",
        Some(env!("CARGO_PKG_VERSION")),
    );
    about.set_version(env!("CARGO_PKG_VERSION"));
    about.set_developers(&["jic8 https://github.com/jic8"]);
    about.set_copyright("© 2026 jic8");
    let credits = gettext("translator-credits");
    if credits != "translator-credits" {
        about.set_translator_credits(&credits);
    }
    about.present(app.active_window().as_ref());
}
