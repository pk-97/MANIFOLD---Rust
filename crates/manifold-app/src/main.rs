mod app;
mod window_registry;
mod frame_timer;
mod ui_root;
mod ui_bridge;
mod text_input;
mod transport_state;
mod input_handler;
mod editing_host;

fn main() {
    env_logger::init();
    log::info!("MANIFOLD starting...");

    let event_loop = winit::event_loop::EventLoop::new().unwrap();
    event_loop.set_control_flow(winit::event_loop::ControlFlow::Poll);

    let mut application = app::Application::new();
    event_loop.run_app(&mut application).unwrap();
}
