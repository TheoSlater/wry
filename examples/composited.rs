use tao::{
  event::{Event, WindowEvent},
  event_loop::{ControlFlow, EventLoop},
  window::WindowBuilder,
};
#[cfg(target_os = "linux")]
use wry::HitTestMode;
#[cfg(target_os = "windows")]
use wry::{
  dpi::{LogicalPosition, LogicalSize},
  Rect,
};
use wry::{WebViewBuilder, WebViewRenderMode};

fn main() -> wry::Result<()> {
  let event_loop = EventLoop::new();
  let window = WindowBuilder::new()
    .with_title("Wry composited")
    .build(&event_loop)
    .unwrap();

  #[cfg(target_os = "linux")]
  let webview = {
    use gtk::prelude::*;
    use tao::platform::unix::WindowExtUnix;
    use wry::WebViewBuilderExtUnix;

    let overlay = gtk::Overlay::new();
    let button = gtk::Button::with_label("Native overlay");
    button.set_halign(gtk::Align::End);
    button.set_valign(gtk::Align::Start);
    button.set_margin_top(24);
    button.set_margin_end(24);
    overlay.add_overlay(&button);
    window
      .default_vbox()
      .unwrap()
      .pack_start(&overlay, true, true, 0);
    overlay.show_all();

    WebViewBuilder::new()
      .with_render_mode(WebViewRenderMode::Composited)
      .with_hit_test_mode(HitTestMode::Normal)
      .with_url("https://tauri.app")
      .build_gtk(&overlay)?
  };

  #[cfg(not(target_os = "linux"))]
  let webview = WebViewBuilder::new()
    .with_render_mode(WebViewRenderMode::Composited)
    .with_url("https://tauri.app")
    .build(&window)?;

  #[cfg(target_os = "windows")]
  let _overlay = WebViewBuilder::new()
    .with_render_mode(WebViewRenderMode::Composited)
    .with_bounds(Rect {
      position: LogicalPosition::new(24, 24).into(),
      size: LogicalSize::new(320, 160).into(),
    })
    .with_html("<body style='background:#20242b;color:white;font:20px sans-serif'><button>Native visual overlay</button></body>")
    .build(&window)?;

  event_loop.run(move |event, _, control_flow| {
    *control_flow = ControlFlow::Wait;
    if let Event::WindowEvent {
      event: WindowEvent::CloseRequested,
      ..
    } = event
    {
      *control_flow = ControlFlow::Exit;
    }
    let _ = &webview;
  });
}
