//! MPRIS — стандартный D-Bus-интерфейс медиаплееров. Через него панель задач,
//! виджеты рабочего стола и медиаклавиши управляют воспроизведением.

use std::rc::{Rc, Weak};

use gtk::prelude::*;
use gtk::{gio, glib};

const BUS_NAME: &str = "org.mpris.MediaPlayer2.Volna";
const OBJECT_PATH: &str = "/org/mpris/MediaPlayer2";
const ROOT_IFACE: &str = "org.mpris.MediaPlayer2";
const PLAYER_IFACE: &str = "org.mpris.MediaPlayer2.Player";
const NO_TRACK: &str = "/org/mpris/MediaPlayer2/TrackList/NoTrack";

pub const MIN_RATE: f64 = 0.5;
pub const MAX_RATE: f64 = 2.0;

const INTROSPECTION: &str = r#"
<node>
  <interface name="org.mpris.MediaPlayer2">
    <method name="Raise"/>
    <method name="Quit"/>
    <property name="CanQuit" type="b" access="read"/>
    <property name="CanRaise" type="b" access="read"/>
    <property name="HasTrackList" type="b" access="read"/>
    <property name="Identity" type="s" access="read"/>
    <property name="DesktopEntry" type="s" access="read"/>
    <property name="SupportedUriSchemes" type="as" access="read"/>
    <property name="SupportedMimeTypes" type="as" access="read"/>
  </interface>
  <interface name="org.mpris.MediaPlayer2.Player">
    <method name="Next"/>
    <method name="Previous"/>
    <method name="Pause"/>
    <method name="PlayPause"/>
    <method name="Stop"/>
    <method name="Play"/>
    <method name="Seek">
      <arg direction="in" name="Offset" type="x"/>
    </method>
    <method name="SetPosition">
      <arg direction="in" name="TrackId" type="o"/>
      <arg direction="in" name="Position" type="x"/>
    </method>
    <method name="OpenUri">
      <arg direction="in" name="Uri" type="s"/>
    </method>
    <signal name="Seeked">
      <arg name="Position" type="x"/>
    </signal>
    <property name="PlaybackStatus" type="s" access="read"/>
    <property name="Rate" type="d" access="readwrite"/>
    <property name="Metadata" type="a{sv}" access="read"/>
    <property name="Volume" type="d" access="readwrite"/>
    <property name="Position" type="x" access="read"/>
    <property name="MinimumRate" type="d" access="read"/>
    <property name="MaximumRate" type="d" access="read"/>
    <property name="CanGoNext" type="b" access="read"/>
    <property name="CanGoPrevious" type="b" access="read"/>
    <property name="CanPlay" type="b" access="read"/>
    <property name="CanPause" type="b" access="read"/>
    <property name="CanSeek" type="b" access="read"/>
    <property name="CanControl" type="b" access="read"/>
  </interface>
</node>
"#;

const MIME_TYPES: &[&str] = &[
    "audio/mpeg", "audio/flac", "audio/x-flac", "audio/ogg", "audio/x-vorbis+ogg", "audio/opus",
    "audio/mp4", "audio/x-m4a", "audio/aac", "audio/wav", "audio/x-wav", "audio/x-mpegurl",
];

#[derive(Clone, Copy, PartialEq)]
pub enum Status {
    Playing,
    Paused,
    Stopped,
}

pub struct Track {
    pub id: u64,
    pub title: String,
    pub artist: Option<String>,
    pub uri: String,
    pub length: Option<gst::ClockTime>,
}

pub enum Command {
    Raise,
    Quit,
    Play,
    Pause,
    PlayPause,
    Stop,
    Next,
    Previous,
    /// Сдвиг в микросекундах, может быть отрицательным
    Seek(i64),
    /// Абсолютная позиция в микросекундах
    SetPosition(i64),
    OpenUri(String),
    SetVolume(f64),
    SetRate(f64),
}

/// Состояние плеера и обработка команд — реализует окно.
pub trait Backend {
    fn status(&self) -> Status;
    fn track(&self) -> Option<Track>;
    fn position(&self) -> gst::ClockTime;
    fn volume(&self) -> f64;
    fn rate(&self) -> f64;
    fn can_go_next(&self) -> bool;
    fn can_go_previous(&self) -> bool;
    fn command(self: Rc<Self>, command: Command);
}

pub struct Mpris {
    connection: gio::DBusConnection,
    registrations: Vec<gio::RegistrationId>,
    backend: Weak<dyn Backend>,
}

impl Mpris {
    pub fn new(connection: gio::DBusConnection, backend: Weak<dyn Backend>) -> Result<Self, glib::Error> {
        let node = gio::DBusNodeInfo::for_xml(INTROSPECTION)?;
        let mut registrations = Vec::new();

        for iface in [ROOT_IFACE, PLAYER_IFACE] {
            let info = node.lookup_interface(iface).expect("interface in introspection");
            let (b_call, b_get, b_set) = (backend.clone(), backend.clone(), backend.clone());
            let id = connection
                .register_object(OBJECT_PATH, &info)
                .method_call(move |_, _, _, _, method, params, invocation| {
                    if let Some(backend) = b_call.upgrade() {
                        if let Some(command) = parse_command(method, &params) {
                            backend.command(command);
                        }
                    }
                    invocation.return_value(None);
                })
                .property(move |_, _, _, _, property| match b_get.upgrade() {
                    Some(backend) => property_value(backend.as_ref(), property),
                    None => default_value(property),
                })
                .set_property(move |_, _, _, _, property, value| {
                    let (Some(backend), Some(v)) = (b_set.upgrade(), value.get::<f64>()) else {
                        return false;
                    };
                    match property {
                        "Volume" => backend.command(Command::SetVolume(v.clamp(0.0, 1.0))),
                        "Rate" if v > 0.0 => backend.command(Command::SetRate(v.clamp(MIN_RATE, MAX_RATE))),
                        _ => return false,
                    }
                    true
                })
                .build()?;
            registrations.push(id);
        }

        // RequestName напрямую: у gio::bus_own_name обработчик потери имени
        // паникует, если шина закрылась раньше приложения
        connection.call(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "RequestName",
            Some(&(BUS_NAME, 0u32).to_variant()),
            None,
            gio::DBusCallFlags::NONE,
            -1,
            None::<&gio::Cancellable>,
            |result| {
                if let Err(err) = result {
                    eprintln!("MPRIS: could not own {BUS_NAME}: {err}");
                }
            },
        );

        Ok(Self {
            connection,
            registrations,
            backend,
        })
    }

    /// Сообщает подписчикам об изменении свойств интерфейса Player.
    pub fn notify(&self, properties: &[&str]) {
        let Some(backend) = self.backend.upgrade() else { return };
        let changed = glib::VariantDict::new(None);
        for &p in properties {
            changed.insert_value(p, &property_value(backend.as_ref(), p));
        }
        let params = glib::Variant::tuple_from_iter([
            PLAYER_IFACE.to_variant(),
            changed.end(),
            Vec::<String>::new().to_variant(),
        ]);
        let _ = self.connection.emit_signal(
            None,
            OBJECT_PATH,
            "org.freedesktop.DBus.Properties",
            "PropertiesChanged",
            Some(&params),
        );
    }

    pub fn seeked(&self, position: gst::ClockTime) {
        let params = (micros(position),).to_variant();
        let _ = self
            .connection
            .emit_signal(None, OBJECT_PATH, PLAYER_IFACE, "Seeked", Some(&params));
    }
}

impl Drop for Mpris {
    fn drop(&mut self) {
        let _ = self.connection.call_sync(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "ReleaseName",
            Some(&(BUS_NAME,).to_variant()),
            None,
            gio::DBusCallFlags::NONE,
            500,
            None::<&gio::Cancellable>,
        );
        for id in self.registrations.drain(..) {
            let _ = self.connection.unregister_object(id);
        }
    }
}

fn parse_command(method: &str, params: &glib::Variant) -> Option<Command> {
    Some(match method {
        "Raise" => Command::Raise,
        "Quit" => Command::Quit,
        "Next" => Command::Next,
        "Previous" => Command::Previous,
        "Pause" => Command::Pause,
        "PlayPause" => Command::PlayPause,
        "Stop" => Command::Stop,
        "Play" => Command::Play,
        "Seek" => Command::Seek(params.get::<(i64,)>()?.0),
        "SetPosition" => Command::SetPosition(params.child_value(1).get::<i64>()?),
        "OpenUri" => Command::OpenUri(params.get::<(String,)>()?.0),
        _ => return None,
    })
}

fn property_value(backend: &dyn Backend, property: &str) -> glib::Variant {
    match property {
        "CanQuit" | "CanRaise" | "CanControl" => true.to_variant(),
        "HasTrackList" => false.to_variant(),
        "Identity" => glib::application_name().unwrap_or_else(|| "Volna".into()).to_variant(),
        "DesktopEntry" => "io.github.jic8.Volna".to_variant(),
        "SupportedUriSchemes" => vec!["file"].to_variant(),
        "SupportedMimeTypes" => MIME_TYPES.to_vec().to_variant(),
        "PlaybackStatus" => match backend.status() {
            Status::Playing => "Playing",
            Status::Paused => "Paused",
            Status::Stopped => "Stopped",
        }
        .to_variant(),
        "Rate" => backend.rate().to_variant(),
        "MinimumRate" => MIN_RATE.to_variant(),
        "MaximumRate" => MAX_RATE.to_variant(),
        "Volume" => backend.volume().to_variant(),
        "Position" => micros(backend.position()).to_variant(),
        "Metadata" => metadata(backend.track()),
        "CanGoNext" => backend.can_go_next().to_variant(),
        "CanGoPrevious" => backend.can_go_previous().to_variant(),
        "CanPlay" | "CanPause" | "CanSeek" => (backend.track().is_some()).to_variant(),
        other => default_value(other),
    }
}

fn default_value(property: &str) -> glib::Variant {
    match property {
        "Metadata" => metadata(None),
        "PlaybackStatus" => "Stopped".to_variant(),
        "Position" => 0i64.to_variant(),
        "Rate" | "Volume" | "MinimumRate" | "MaximumRate" => 1.0f64.to_variant(),
        _ => false.to_variant(),
    }
}

fn metadata(track: Option<Track>) -> glib::Variant {
    let dict = glib::VariantDict::new(None);
    let path = track
        .as_ref()
        .map_or(NO_TRACK.to_owned(), |t| format!("/io/github/jic8/Volna/Track/{}", t.id));
    if let Ok(path) = glib::variant::ObjectPath::try_from(path) {
        dict.insert_value("mpris:trackid", &path.to_variant());
    }
    if let Some(t) = track {
        dict.insert_value("xesam:title", &t.title.to_variant());
        if let Some(artist) = t.artist {
            dict.insert_value("xesam:artist", &vec![artist].to_variant());
        }
        dict.insert_value("xesam:url", &t.uri.to_variant());
        if let Some(length) = t.length {
            dict.insert_value("mpris:length", &micros(length).to_variant());
        }
    }
    dict.end()
}

fn micros(t: gst::ClockTime) -> i64 {
    (t.nseconds() / 1000) as i64
}
