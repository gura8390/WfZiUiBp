use std::cell::Cell;

use anyhow::{Context, Result, ensure};
use eframe::{AppCreator, EframeWinitApplication, NativeOptions, UserEvent};
use winit::{
    application::ApplicationHandler,
    event::{DeviceEvent, DeviceId, StartCause, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    window::WindowId,
};

pub(super) fn run(title: &str, options: NativeOptions, create: AppCreator<'_>) -> Result<()> {
    let event_loop = EventLoop::<UserEvent>::with_user_event()
        .build()
        .context("无法创建窗口事件循环")?;
    let created = Cell::new(false);
    let app = eframe::create_native(
        title,
        options,
        Box::new(|context| {
            let app = create(context)?;
            created.set(true);
            Ok(app)
        }),
        &event_loop,
    );
    event_loop
        .run_app(&mut WindowsEventLoop(app))
        .context("窗口事件循环失败")?;
    ensure!(created.get(), "无法初始化文字识别主窗口");
    Ok(())
}

struct WindowsEventLoop<'a>(EframeWinitApplication<'a>);

impl ApplicationHandler<UserEvent> for WindowsEventLoop<'_> {
    fn new_events(&mut self, event_loop: &ActiveEventLoop, cause: StartCause) {
        self.0.new_events(event_loop, cause);
    }

    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        self.0.resumed(event_loop);
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        self.0.user_event(event_loop, event);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        self.0.window_event(event_loop, id, event);
    }

    fn device_event(&mut self, event_loop: &ActiveEventLoop, id: DeviceId, event: DeviceEvent) {
        self.0.device_event(event_loop, id, event);
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.0.about_to_wait(event_loop);
        // eframe 0.33 can leave Poll set when a hidden HWND never delivers
        // the requested redraw. Native messages can wake Wait on their own.
        if event_loop.control_flow() == ControlFlow::Poll {
            event_loop.set_control_flow(ControlFlow::Wait);
        }
    }

    fn suspended(&mut self, event_loop: &ActiveEventLoop) {
        self.0.suspended(event_loop);
    }

    fn exiting(&mut self, event_loop: &ActiveEventLoop) {
        self.0.exiting(event_loop);
    }

    fn memory_warning(&mut self, event_loop: &ActiveEventLoop) {
        self.0.memory_warning(event_loop);
    }
}
