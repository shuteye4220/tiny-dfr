# tiny-dfr
The most basic dynamic function row daemon possible


## Dependencies
cairo, libinput, freetype, fontconfig, librsvg 2.59 or later, uinput enabled in kernel config

## Screen reader support

tiny-dfr can be used without sight. In touch exploration mode the bar
behaves much like the Touch Bar does under VoiceOver:

* Touch the bar, or slide a finger along it, to hear the button under your
  finger. Nothing is pressed.
* Double-tap anywhere on the bar to press the button you last heard. Keep
  your finger down on the second tap to hold the key, for example to keep
  turning the volume up.
* Or split-tap: keep one finger on the button and tap anywhere with another
  finger. Tap again to press it again.
* A quick double press of Fn, if `DoublePressSwitchLayers` is set, announces
  the layer it switched to.

The focused button also gets a white outline. Touch exploration works while
the bar is dimmed or dark, so there is no need to wake it first.

Speech comes from `tiny-dfr-speech`, a small helper that runs in your
session and passes announcements to Orca (version 49 or later with
python-dasbus, so they also reach a braille display) or otherwise to Speech
Dispatcher. It needs Python 3 and PyGObject, both of which Orca already
requires. By default the helper connects only while a screen reader is
running, and the bar is in touch exploration mode only while the helper is
connected, so turning Orca on and off turns touch exploration on and off.

To install it:

    install -Dm755 a11y/tiny-dfr-speech /usr/bin/tiny-dfr-speech
    install -Dm644 etc/systemd/user/tiny-dfr-speech.service \
        /usr/lib/systemd/user/tiny-dfr-speech.service
    systemctl --user enable --now tiny-dfr-speech

Useful options, see `tiny-dfr-speech --help`: `--when always` keeps touch
exploration on with no screen reader running, `--on-activate silent` stops
button names being repeated when pressed, and `--speech print -v` shows what
would be spoken, for troubleshooting.

In `config.toml`, `TouchExplore` can force the mode on or off, and each
button takes an optional `Label` giving the name to announce.

The daemon side is a Unix socket, `/run/tiny-dfr/a11y.sock`, carrying one
JSON object per line (`focus`, `activate` and `layer` events, each with a
`label`). The systemd unit needs `RuntimeDirectory=tiny-dfr` for it. Only
root and the user active on a local seat, according to logind, may connect;
on systems without logind any local user may.

## License

tiny-dfr is licensed under the MIT license, as included in the [LICENSE](LICENSE) file.

* Copyright The Asahi Linux Contributors

Please see the Git history for authorship information.

tiny-dfr embeds Google's [material-design-icons](https://github.com/google/material-design-icons)
which are licensed under [Apache License Version 2.0](LICENSE.material)
Some icons are derivatives of material-icons, with edits made by kekrby.
