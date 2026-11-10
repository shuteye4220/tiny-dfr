use crate::fonts::{FontConfig, Pattern};
use crate::FunctionLayer;
use anyhow::Error;
use cairo::FontFace;
use freetype::Library as FtLibrary;
use input_linux::Key;
use nix::{
    errno::Errno,
    sys::inotify::{AddWatchFlags, InitFlags, Inotify, InotifyEvent, WatchDescriptor},
};
use serde::{
    de::{self, Visitor},
    Deserialize, Deserializer,
};
use std::{fmt, fs::read_to_string, os::fd::AsFd};

const USER_CFG_PATH: &str = "/etc/tiny-dfr/config.toml";

pub struct Config {
    pub show_button_outlines: bool,
    pub enable_pixel_shift: bool,
    pub font_face: FontFace,
    pub adaptive_brightness: bool,
    pub active_brightness: u32,
    pub double_press_switch_layers: u32,
    pub touch_explore: TouchExplore,
}

/// When the Touch Bar is in touch exploration (screen reader) mode.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TouchExplore {
    /// While an accessibility client is connected.
    Auto,
    Always,
    Never,
}

impl TouchExplore {
    fn parse(value: &str) -> TouchExplore {
        match value.to_ascii_lowercase().as_str() {
            "auto" => TouchExplore::Auto,
            "always" => TouchExplore::Always,
            "never" => TouchExplore::Never,
            other => {
                // Not worth taking the whole bar down over.
                eprintln!("Invalid TouchExplore value {other:?}, using \"auto\"");
                TouchExplore::Auto
            }
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct ConfigProxy {
    media_layer_default: Option<bool>,
    show_button_outlines: Option<bool>,
    enable_pixel_shift: Option<bool>,
    font_template: Option<String>,
    adaptive_brightness: Option<bool>,
    active_brightness: Option<u32>,
    double_press_switch_layers: Option<u32>,
    touch_explore: Option<String>,
    primary_layer_keys: Option<Vec<ButtonConfig>>,
    media_layer_keys: Option<Vec<ButtonConfig>>,
}

fn array_or_single<'de, D>(deserializer: D) -> Result<Vec<Key>, D::Error>
where
    D: Deserializer<'de>,
{
    struct ArrayOrSingle;

    impl<'de> Visitor<'de> for ArrayOrSingle {
        type Value = Vec<Key>;

        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
            f.write_str("string or array of strings")
        }

        fn visit_str<E: de::Error>(self, value: &str) -> Result<Vec<Key>, E> {
            Ok(vec![Deserialize::deserialize(
                de::value::BorrowedStrDeserializer::new(value),
            )?])
        }

        fn visit_seq<A: de::SeqAccess<'de>>(self, seq: A) -> Result<Vec<Key>, A::Error> {
            Deserialize::deserialize(de::value::SeqAccessDeserializer::new(seq))
        }
    }

    deserializer.deserialize_any(ArrayOrSingle)
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct ButtonConfig {
    #[serde(alias = "Svg")]
    pub icon: Option<String>,
    pub text: Option<String>,
    /// Name announced by screen readers; derived from the button if unset.
    pub label: Option<String>,
    pub theme: Option<String>,
    pub time: Option<String>,
    pub battery: Option<String>,
    pub locale: Option<String>,
    #[serde(deserialize_with = "array_or_single", default)]
    pub action: Vec<Key>,
    pub stretch: Option<usize>,
    pub icon_width: Option<i32>,
    pub icon_height: Option<i32>,
}

fn load_font(name: &str) -> FontFace {
    let fontconfig = FontConfig::new();
    let mut pattern = Pattern::new(name);
    fontconfig.perform_substitutions(&mut pattern);
    let pat_match = match fontconfig.match_pattern(&pattern) {
        Ok(pat) => pat,
        Err(_) => panic!("Unable to find specified font. If you are using the default config, make sure you have at least one font installed")
    };
    let file_name = pat_match.get_file_name();
    let file_idx = pat_match.get_font_index();
    let ft_library = FtLibrary::init().unwrap();
    let face = ft_library.new_face(file_name, file_idx).unwrap();
    FontFace::create_from_ft(&face).unwrap()
}

fn load_config(width: u16) -> (Config, [FunctionLayer; 2]) {
    let mut base =
        toml::from_str::<ConfigProxy>(&read_to_string("/usr/share/tiny-dfr/config.toml").unwrap())
            .unwrap();
    let user = read_to_string(USER_CFG_PATH)
        .map_err::<Error, _>(|e| e.into())
        .and_then(|r| Ok(toml::from_str::<ConfigProxy>(&r)?));
    if let Ok(user) = user {
        base.media_layer_default = user.media_layer_default.or(base.media_layer_default);
        base.show_button_outlines = user.show_button_outlines.or(base.show_button_outlines);
        base.enable_pixel_shift = user.enable_pixel_shift.or(base.enable_pixel_shift);
        base.font_template = user.font_template.or(base.font_template);
        base.adaptive_brightness = user.adaptive_brightness.or(base.adaptive_brightness);
        base.media_layer_keys = user.media_layer_keys.or(base.media_layer_keys);
        base.primary_layer_keys = user.primary_layer_keys.or(base.primary_layer_keys);
        base.active_brightness = user.active_brightness.or(base.active_brightness);
        base.double_press_switch_layers = user.double_press_switch_layers.or(base.double_press_switch_layers);
        base.touch_explore = user.touch_explore.or(base.touch_explore);
    };
    let mut media_layer_keys = base.media_layer_keys.unwrap();
    let mut primary_layer_keys = base.primary_layer_keys.unwrap();
    if width >= 2170 {
        for layer in [&mut media_layer_keys, &mut primary_layer_keys] {
            layer.insert(
                0,
                ButtonConfig {
                    icon: None,
                    text: Some("esc".into()),
                    label: None,
                    theme: None,
                    action: vec![Key::Esc],
                    stretch: None,
                    time: None,
                    locale: None,
                    battery: None,
                    icon_width: None,
                    icon_height: None,
                },
            );
        }
    }
    let media_layer = FunctionLayer::with_config(media_layer_keys, "Media keys");
    let fkey_layer = FunctionLayer::with_config(primary_layer_keys, "Function keys");
    let layers = if base.media_layer_default.unwrap() {
        [media_layer, fkey_layer]
    } else {
        [fkey_layer, media_layer]
    };
    let cfg = Config {
        show_button_outlines: base.show_button_outlines.unwrap(),
        enable_pixel_shift: base.enable_pixel_shift.unwrap(),
        adaptive_brightness: base.adaptive_brightness.unwrap(),
        font_face: load_font(&base.font_template.unwrap()),
        active_brightness: base.active_brightness.unwrap(),
        double_press_switch_layers: base.double_press_switch_layers.unwrap(),
        // Optional so that an older /usr/share config keeps working.
        touch_explore: base
            .touch_explore
            .as_deref()
            .map_or(TouchExplore::Auto, TouchExplore::parse),
    };
    (cfg, layers)
}

pub struct ConfigManager {
    inotify_fd: Inotify,
    watch_desc: Option<WatchDescriptor>,
}

fn arm_inotify(inotify_fd: &Inotify) -> Option<WatchDescriptor> {
    let flags = AddWatchFlags::IN_MOVED_TO | AddWatchFlags::IN_CLOSE | AddWatchFlags::IN_ONESHOT;
    match inotify_fd.add_watch(USER_CFG_PATH, flags) {
        Ok(wd) => Some(wd),
        Err(Errno::ENOENT) => None,
        e => Some(e.unwrap()),
    }
}

impl ConfigManager {
    pub fn new() -> ConfigManager {
        let inotify_fd = Inotify::init(InitFlags::IN_NONBLOCK).unwrap();
        let watch_desc = arm_inotify(&inotify_fd);
        ConfigManager {
            inotify_fd,
            watch_desc,
        }
    }
    pub fn load_config(&self, width: u16) -> (Config, [FunctionLayer; 2]) {
        load_config(width)
    }
    pub fn update_config(
        &mut self,
        cfg: &mut Config,
        layers: &mut [FunctionLayer; 2],
        width: u16,
    ) -> bool {
        if self.watch_desc.is_none() {
            self.watch_desc = arm_inotify(&self.inotify_fd);
            return false;
        }
        match self.inotify_fd.read_events() {
            Err(Errno::EAGAIN) => false,
            r => self.handle_events(cfg, layers, width, r),
        }
    }
    #[cold]
    fn handle_events(&mut self, cfg: &mut Config, layers: &mut [FunctionLayer; 2], width: u16, evts: Result<Vec<InotifyEvent>, Errno>) -> bool {
        let mut ret = false;
        for evt in evts.unwrap() {
            if Some(evt.wd) != self.watch_desc {
                continue;
            }
            let parts = load_config(width);
            *cfg = parts.0;
            *layers = parts.1;
            ret = true;
            self.watch_desc = arm_inotify(&self.inotify_fd);
        }
        ret
    }
    pub fn fd(&self) -> &impl AsFd {
        &self.inotify_fd
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shipped_config_parses_with_touch_explore() {
        let cfg: ConfigProxy = toml::from_str(include_str!("../share/tiny-dfr/config.toml")).unwrap();
        assert_eq!(cfg.touch_explore.as_deref(), Some("auto"));
        assert!(cfg.primary_layer_keys.unwrap().len() >= 12);
    }

    #[test]
    fn label_and_touch_explore_are_accepted() {
        let cfg: ConfigProxy = toml::from_str(
            r#"
            TouchExplore = "Never"
            MediaLayerKeys = [ { Icon = "search", Action = "Search", Label = "Find" } ]
            "#,
        )
        .unwrap();
        assert_eq!(cfg.media_layer_keys.unwrap()[0].label.as_deref(), Some("Find"));
        assert_eq!(
            TouchExplore::parse(&cfg.touch_explore.unwrap()),
            TouchExplore::Never
        );
        assert_eq!(TouchExplore::parse("bogus"), TouchExplore::Auto);
    }
}
