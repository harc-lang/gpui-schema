use gpui::{actions, App, AppContext as _, Application, KeyBinding, Menu, MenuItem, WindowOptions, px, size};
use gpui_component::Root;
use schema_gui::{NodeFilter, SchemaForm};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

actions!(demo, [Quit]);

#[derive(Debug, Clone, JsonSchema, Serialize, Deserialize)]
struct AppConfig {
    /// The application name
    app_name: String,
    /// Server configuration
    server: ServerConfig,
    /// Logging settings
    logging: LogConfig,
    /// Run mode
    mode: RunMode,
    /// Maximum retry count
    max_retries: u32,
    /// Timeout in seconds
    timeout: f64,
    /// Enable experimental features
    experimental: bool,
    /// Optional description
    description: Option<String>,
    /// Optional nickname
    nickname: Option<String>,
    /// Optional backup server
    backup_server: Option<ServerConfig>,
    /// Enabled features
    features: Vec<Feature>,
}

#[derive(Debug, Clone, JsonSchema, Serialize, Deserialize)]
struct ServerConfig {
    /// Hostname to bind to
    hostname: String,
    /// Port number
    port: u16,
    /// Use TLS
    tls: bool,
}

#[derive(Debug, Clone, JsonSchema, Serialize, Deserialize)]
struct LogConfig {
    /// Log level
    level: LogLevel,
    /// Log file path
    file: Option<String>,
    /// Use colors in output
    use_color: bool,
}

#[derive(Debug, Clone, JsonSchema, Serialize, Deserialize)]
enum RunMode {
    Debug,
    Release,
    Test,
}

#[derive(Debug, Clone, JsonSchema, Serialize, Deserialize)]
enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

#[derive(Debug, Clone, JsonSchema, Serialize, Deserialize, PartialEq)]
enum Feature {
    Logging,
    Metrics,
    Tracing,
    Auth,
    Caching,
}

/// Example filter: makes `app_name` and `experimental` read-only,
/// and disables choosing individual enum variants under `mode`.
struct DemoFilter;

impl NodeFilter for DemoFilter {
    fn enabled(&self, path: &str) -> bool {
        !matches!(
            path,
            "app_name" | "experimental" | "mode.Debug" | "mode.Release" | "mode.Test"
        )
    }
}

fn main() {
    let config = AppConfig {
        app_name: "my-app".into(),
        server: ServerConfig {
            hostname: "localhost".into(),
            port: 8080,
            tls: false,
        },
        logging: LogConfig {
            level: LogLevel::Info,
            file: None,
            use_color: true,
        },
        mode: RunMode::Release,
        max_retries: 3,
        timeout: 30.0,
        experimental: false,
        description: Some("A sample application".into()),
        nickname: None,
        backup_server: Some(ServerConfig {
            hostname: "backup.local".into(),
            port: 9090,
            tls: true,
        }),
        features: vec![Feature::Logging, Feature::Auth],
    };

    let schema = schemars::schema_for!(AppConfig);
    let value = serde_json::to_value(&config).unwrap();

    let app = Application::new();

    app.run(move |cx: &mut App| {
        gpui_component::init(cx);

        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.bind_keys([KeyBinding::new("cmd-q", Quit, None)]);
        cx.set_menus(vec![Menu {
            name: "Schema GUI".into(),
            items: vec![MenuItem::action("Quit Schema GUI", Quit)],
        }]);

        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        cx.spawn(async move |cx| {
            let window_options = cx.update(|cx| WindowOptions {
                window_bounds: Some(gpui::WindowBounds::Windowed(gpui::Bounds::centered(
                    None,
                    size(px(600.0), px(800.0)),
                    cx,
                ))),
                ..Default::default()
            })?;

            let window = cx.open_window(
                window_options,
                |window, cx| {
                    let form = cx.new(|cx| {
                        let mut form = SchemaForm::new(&schema, &value, window, cx);
                        form.set_filter(DemoFilter, window, cx);
                        form
                    });
                    cx.new(|cx| Root::new(form, window, cx))
                },
            )?;

            window.update(cx, |_, window, _| {
                window.activate_window();
            })?;

            cx.update(|cx| cx.activate(true))?;

            Ok::<_, anyhow::Error>(())
        })
        .detach();
    });
}
