//! Ferrogram entry point.

use ferrogram::app::App;
use ferrogram::config::{APP_ID, Config};
use ferrogram::ui::theme::{FONT, INTER_REGULAR, INTER_SEMIBOLD};
use iced::{Size, window};

fn main() -> iced::Result {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!(
            "Ferrogram — unofficial Telegram client\n\n\
             USAGE: ferrogram [--mock] [--screenshot <screen> <out.png>]\n\n\
             screens: login-phone login-code login-password chats search reply media\n\
             env: FERROGRAM_API_ID, FERROGRAM_API_HASH, FERRO_THEME=light, FERRO_TRACE_STARTUP=1"
        );
        return Ok(());
    }
    // Renderer choice (architecture §1): the CPU renderer (tiny-skia) keeps
    // idle RSS around 30 MB, while wgpu costs 100+ MB for its device and
    // pipelines. Default to tiny-skia; `FERRO_RENDERER=wgpu` or an explicit
    // `ICED_BACKEND` opts back into the GPU path.
    if std::env::var_os("ICED_BACKEND").is_none()
        && std::env::var("FERRO_RENDERER").as_deref() != Ok("wgpu")
    {
        // SAFETY: called at the top of `main`, before any thread is spawned.
        unsafe { std::env::set_var("ICED_BACKEND", "tiny-skia") };
    }
    let config = Config::from_env(&args);
    ferrogram::logging::init((!config.mock).then(|| config.log_path()).as_deref());
    tracing::info!(
        mock = config.mock,
        "starting Ferrogram {}",
        env!("CARGO_PKG_VERSION")
    );

    let size = if config.screenshot.is_some() {
        Size::new(1280.0, 800.0)
    } else {
        Size::new(1180.0, 760.0)
    };
    let window = window::Settings {
        size,
        min_size: Some(Size::new(720.0, 480.0)),
        resizable: config.screenshot.is_none(),
        #[cfg(target_os = "linux")]
        platform_specific: window::settings::PlatformSpecific {
            application_id: APP_ID.into(),
            ..Default::default()
        },
        ..window::Settings::default()
    };
    #[cfg(not(target_os = "linux"))]
    let _ = APP_ID;

    iced::application(
        move || App::boot(config.clone()),
        App::update,
        ferrogram::ui::view,
    )
    .title(App::title)
    .theme(App::theme)
    .subscription(App::subscription)
    .font(INTER_REGULAR)
    .font(INTER_SEMIBOLD)
    .default_font(FONT)
    .window(window)
    .run()
}
