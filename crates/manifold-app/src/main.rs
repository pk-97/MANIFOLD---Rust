mod app;
mod content_command;
mod content_pipeline;
mod content_state;
mod content_thread;
mod dialog_path_memory;
mod editing_host;
mod frame_timer;
mod input_handler;
mod input_host;
mod project_io;
mod text_input;
mod transport_state;
mod ui_bridge;
mod ui_root;
mod user_prefs;
mod window_registry;

fn main() {
    env_logger::init();
    log::info!("MANIFOLD starting...");

    let event_loop = winit::event_loop::EventLoop::new().unwrap();
    event_loop.set_control_flow(winit::event_loop::ControlFlow::Poll);

    let mut application = app::Application::new();
    event_loop.run_app(&mut application).unwrap();
}
