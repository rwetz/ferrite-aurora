#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod prefs;
mod prefs_store;
mod probe;
mod settings_ui;
mod space;
mod worker;
use ferrite_design::prelude::*;
use gpui::{
    App, AppContext as _, Context, Entity, IntoElement, KeyBinding, Render, Subscription, Task,
    Window, div, px, size,
};

struct Aurora {
    palette: Entity<CommandPalette>,
    settings: Entity<settings_ui::Settings>,
    _appearance: Subscription,
    input: Entity<TextInput>,
    _input: Subscription,
    location: (f32, f32),
    data: Option<space::Space>,
    globe: Option<dither::Picture>,
    status: String,
    busy: bool,
    task: Option<Task<()>>,
}
fn location_path() -> Option<std::path::PathBuf> {
    std::env::var_os(if cfg!(windows) { "APPDATA" } else { "HOME" })
        .map(|base| std::path::PathBuf::from(base).join("ferrite/aurora-location.txt"))
}
impl Aurora {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let location = location_path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| space::coords(&s))
            .unwrap_or((46.88, -96.79));
        let input = cx.new(|cx| {
            let mut input = TextInput::new(window, cx).placeholder("Latitude, longitude");
            input.set_value(format!("{},{}", location.0, location.1), cx);
            input
        });
        let subscription = cx.subscribe(&input, |this: &mut Self, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Submit) {
                this.set_location(cx);
            }
        });
        let mut app = Self {
            settings: cx.new(|_| settings_ui::Settings::new()),
            palette: cx.new(|cx| CommandPalette::new(window, cx)),
            _appearance: theme::follow_system(window),
            input,
            _input: subscription,
            location,
            data: None,
            globe: None,
            status: "Connecting to NOAA…".into(),
            busy: false,
            task: None,
        };
        let weak = cx.weak_entity();
        app.palette.update(cx, |palette, cx| {
            let mut commands = Vec::new();
            for scheme in SCHEMES {
                commands.push(
                    command(format!("Scheme: {}", scheme.name))
                        .group("Theme")
                        .on_run(move |_, cx| prefs::scheme(scheme, cx)),
                );
            }
            commands.push(
                command("Toggle light / dark")
                    .group("Theme")
                    .on_run(|window, cx| {
                        prefs::appearance(
                            if theme::appearance(cx) == Appearance::Light {
                                Appearance::Dark
                            } else {
                                Appearance::Light
                            },
                            window,
                            cx,
                        );
                    }),
            );
            commands.push(
                command("Refresh NOAA")
                    .group("Aurora")
                    .on_run(move |_, cx| {
                        let _ = weak.update(cx, |this, cx| this.refresh(cx));
                    }),
            );
            commands.push(settings_ui::settings_command(app.settings.clone()));
            palette.set_commands(commands, cx);
        });
        app.refresh(cx);
        app
    }
    fn set_location(&mut self, cx: &mut Context<Self>) {
        let Some(location) = space::coords(&self.input.read(cx).value()) else {
            self.status = "Use valid latitude,longitude coordinates".into();
            cx.notify();
            return;
        };
        self.location = location;
        if let Some(path) = location_path() {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Err(e) = std::fs::write(path, format!("{},{}", location.0, location.1)) {
                self.status = format!("Couldn't save location: {e}");
            }
        }
        if let Some(data) = &self.data {
            self.globe = Some(dither::Picture::new(
                480,
                480,
                space::globe(&data.grid, location),
            ));
        }
        cx.notify();
    }
    fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.status = "Fetching NOAA space weather…".into();
        self.busy = true;
        self.task = Some(cx.spawn(async move |this, cx| {
            loop {
                let result = worker::run(space::fetch).await;
                if this
                    .update(cx, |this, cx| {
                        this.busy = false;
                        match result {
                            Ok(data) => {
                                this.globe = (!data.grid.is_empty()).then(|| {
                                    dither::Picture::new(
                                        480,
                                        480,
                                        space::globe(&data.grid, this.location),
                                    )
                                });
                                this.status = if data.notes.is_empty() {
                                    "Live NOAA data · refreshes every five minutes".into()
                                } else {
                                    data.notes.join(" · ")
                                };
                                this.data = Some(data);
                            }
                            Err(error) => this.status = error,
                        }
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(300))
                    .await;
            }
        }));
        cx.notify();
    }
}
fn number(value: Option<f32>) -> String {
    value
        .map(|v| format!("{v:.1}"))
        .unwrap_or_else(|| "Unavailable".into())
}
impl Render for Aurora {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let mut content = div()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div().flex().gap_2().child(self.input.clone()).child(
                    Button::new("location")
                        .label("Set location")
                        .secondary()
                        .on_click(cx.listener(|this, _, _, cx| this.set_location(cx))),
                ),
            )
            .child(div().p_3().bg(hsla(p.surface)).child(self.status.clone()));
        if let Some(data) = &self.data {
            if self.globe.is_some() {
                probe::record(format!(
                    "NOAA Kp {:.2} · aurora model {}",
                    data.kp, data.forecast_time
                ));
            }
            let readings = panel("Geomagnetic activity")
                .meta(space::storm(data.kp))
                .flex_1()
                .min_w_0()
                .child(
                    div()
                        .display(Scale::X3, window)
                        .text_color(hsla(p.accent))
                        .child(format!("KP {:.2}", data.kp)),
                )
                .child(ascii_gauge(data.kp / 9.).label("KP").cells(30))
                .child(div().child(format!("Reading: {} UTC", data.time)))
                .child(
                    property_list()
                        .row("Solar wind", format!("{} km/s", number(data.speed)))
                        .row("Density", format!("{} /cm³", number(data.density)))
                        .row("Bz GSM", format!("{} nT", number(data.bz))),
                )
                .child(
                    line_chart(
                        "kp-history",
                        data.history.iter().map(|v| v.1).collect::<Vec<_>>(),
                    )
                    .labels(data.history.iter().map(|v| v.0.clone()).collect::<Vec<_>>())
                    .height(px(150.)),
                )
                .child(div().child(format!(
                    "Your coordinates: {:.2}, {:.2}",
                    self.location.0, self.location.1
                )))
                .child(div().child(format!(
                        "Local model intensity: {}",
                        space::intensity(&data.grid, self.location.0, self.location.1)
                            .map(|v| format!("{v}%"))
                            .unwrap_or_else(|| "Unavailable".into())
                    )));
            let viewport = window.viewport_size();
            let globe_h = px((f32::from(viewport.height) - 220.).clamp(240., 430.));
            let mut globe = panel("Aurora globe")
                .meta("NORTH POLAR VIEW")
                .flex_1()
                .min_w_0();
            if let Some(picture) = self.globe.clone() {
                globe = globe.child(
                    responsive("globe-room", move |room, _, cx| {
                        let p = palette(cx);
                        let edge = room.width.min(room.height);
                        div()
                            .size_full()
                            .flex()
                            .justify_center()
                            .items_center()
                            .child(
                                dither(picture.clone())
                                    .w(edge)
                                    .h(edge)
                                    .ink(hsla(p.accent))
                                    .paper(hsla(p.bg)),
                            )
                            .into_any_element()
                    })
                    .w_full()
                    // Square-ish to the panel, but no taller than the window
                    // can show beside the readings.
                    .h(globe_h),
                );
            }
            globe=globe.child(div().child(format!("OVATION forecast: {}",data.forecast_time)))
                .child(div().text_color(hsla(p.fg_dim)).child("Cross = your location in the northern hemisphere. Model intensity is not a guarantee of visible aurora; darkness and cloud cover matter."));
            content = content.child(
                div()
                    .flex()
                    .when(window.viewport_size().width <= px(900.), |el| el.flex_col())
                    .gap_3()
                    .child(readings)
                    .child(globe),
            );
            let mut history = panel("Activity timeline").meta("NOAA / UTC");
            for (time, value) in data.history.iter().rev().take(8) {
                history = history.child(
                    div()
                        .flex()
                        .justify_between()
                        .child(time.clone())
                        .child(format!("Kp {value:.2} · {}", space::storm(*value))),
                );
            }
            content = content.child(history);
        }
        let menu = Button::new("commands")
            .icon(Icon::Menu)
            .ghost()
            .small()
            .on_click(cx.listener(|this, _, window, cx| {
                this.palette.update(cx, |p, cx| p.toggle(window, cx))
            }));
        window_frame().child(power_on_in(
            "power",
            div()
                .size_full()
                .flex()
                .flex_col()
                .bg(hsla(p.bg))
                .text_color(hsla(p.fg))
                .body(text::BASE)
                .on_action(cx.listener(|this, _: &TogglePalette, window, cx| {
                    this.palette.update(cx, |p, cx| p.toggle(window, cx))
                }))
                .on_action(cx.listener(|this, _: &settings_ui::ToggleSettings, _, cx| {
                    this.settings.update(cx, |s, cx| s.toggle(cx));
                }))
                .child(self.settings.clone())
                .child(self.palette.clone())
                .child(
                    title_bar("Aurora")
                        .child(settings_ui::button(self.settings.clone()))
                        .child(menu)
                        .child(
                            Button::new("refresh")
                                .label("Refresh")
                                .ghost()
                                .small()
                                .disabled(self.busy)
                                .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
                        ),
                )
                .child(
                    // A visible bar, so a short window reads as "there's more".
                    scroll_area("content")
                        .flex_1()
                        .min_h_0()
                        .child(div().p_4().child(content)),
                )
                .child(
                    status_bar()
                        .left_live(if self.busy { "LOADING" } else { "NOAA SWPC" })
                        .right(theme::scheme(cx).name.to_uppercase()),
                ),
        ))
    }
}
fn main() {
    if std::env::args().any(|a| a == "--version") {
        println!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
        return;
    }
    if std::env::args().any(|a| a == "--smoke") {
        match space::fetch() {
            Ok(data) if !data.grid.is_empty() => println!(
                "NOAA: Kp {:.2}, {} cells, solar wind {} km/s",
                data.kp,
                data.grid.len(),
                number(data.speed)
            ),
            Ok(_) => {
                eprintln!("No aurora model");
                std::process::exit(1);
            }
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
        return;
    }
    gpui_platform::application().run(|cx: &mut App| {
        prefs::init(cx);
        cx.bind_keys([
            KeyBinding::new("ctrl-shift-p", TogglePalette, None),
            KeyBinding::new("ctrl-,", settings_ui::ToggleSettings, None),
        ]);
        let options =
            chrome::remembered_window_options("aurora", "Aurora", size(px(1180.), px(860.)), cx);
        cx.open_window(options, |window, cx| {
            chrome::square_corners(window);
            chrome::remember_window("aurora", window, cx);
            chrome::power_off_on_close(window, cx);
            cx.new(|cx| Aurora::new(window, cx))
        })
        .expect("Couldn't open Aurora");
        cx.activate(true);
    });
}
