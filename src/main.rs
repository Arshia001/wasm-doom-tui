use std::{
    cell::RefCell,
    rc::Rc,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use ratatui::{
    buffer::Buffer,
    crossterm::event::{self, Event, KeyCode, KeyEventKind},
    layout::Rect,
    style::Stylize,
    symbols::border,
    text::{Line, Text},
    widgets::{Block, Paragraph, Widget},
    DefaultTerminal,
};
use ratatui_image::{picker::Picker, protocol::Protocol, FontSize};
use wasmer::{
    imports, Function, FunctionEnv, FunctionEnvMut, Instance, Memory, MemoryType, Module, Store,
    TypedFunction, WasmSlice,
};

const WASM_BYTES: &[u8] = include_bytes!("../doom.wasm");
const MEMORY_PAGES: u32 = 102;

// This needs to be static so it's accessible to the rendering WASM import function.
// Since we only have one thread, we can safely use an Rc. However, Rust doesn't know
// this, so we need to make it a thread local to keep Rust happy.
thread_local! {
    static TERMINAL: Rc<RefCell<Option<DefaultTerminal>>> = Rc::new(RefCell::new(None));
}

/// The app status, modified in input functions and WASM imports. This
/// is placed inside a Wasmer FunctionEnv.
struct DoomApp {
    exit: bool,

    last_log_line: Option<String>,
    last_log_error: bool,

    image_picker: Picker,
    current_frame: Option<Protocol>,
    default_font_size: FontSize,
    zoom: u16,

    started_at: Instant,
    memory: Memory,

    last_second: Instant,
    frames_since_last_second: u16,
    fps: u16,
}

/// The exported functions we call to control the game's state.
struct DoomFunctions {
    main: TypedFunction<(i32, i32), i32>,
    step: TypedFunction<(), ()>,
    add_event: TypedFunction<(i32, i32), ()>,
}

/// The global state of the application, including the WASM store.
struct DoomGlobalState<'a> {
    store: &'a mut Store,
    env: &'a mut FunctionEnv<DoomApp>,
    functions: DoomFunctions,
}

fn main() -> Result<()> {
    let terminal = ratatui::init();
    TERMINAL.with(move |t| *t.borrow_mut() = Some(terminal));

    let mut store = Store::default();
    let memory = Memory::new(&mut store, MemoryType::new(MEMORY_PAGES, None, false))?;

    let doom_app = {
        let picker = {
            match Picker::from_query_stdio() {
                Ok(picker) => picker,
                Err(ratatui_image::errors::Errors::NoFontSize) => {
                    // Just pick a default at random... needs to be done on Windows
                    Picker::from_fontsize((8, 16))
                }
                e @ Err(_) => {
                    // TODO: is there a better way to do this?
                    _ = e.context("Failed to query terminal's image rendering capabilities")?;
                    unreachable!();
                }
            }
        };

        DoomApp {
            exit: false,

            last_log_line: None,
            last_log_error: false,

            default_font_size: picker.font_size(),
            image_picker: picker,
            current_frame: None,
            zoom: 1,

            started_at: Instant::now(),
            memory: memory.clone(),

            last_second: Instant::now(),
            frames_since_last_second: 0,
            fps: 0,
        }
    };

    let mut env = FunctionEnv::new(&mut store, doom_app);
    let module = Module::new(&store, WASM_BYTES)?;
    let imports = imports! {
        "env" => {
            "memory" => memory,
        },
        "js" => {
            "js_console_log" => Function::new_typed_with_env(&mut store, &env, log_string_normal),
            "js_stdout" => Function::new_typed_with_env(&mut store, &env, log_string_normal),
            "js_stderr" => Function::new_typed_with_env(&mut store, &env, log_string_error),
            "js_milliseconds_since_start" => Function::new_typed_with_env(&mut store, &env, milliseconds_since_start),
            "js_draw_screen" => Function::new_typed_with_env(&mut store, &env, draw_screen),
        },
    };
    let instance = Instance::new(&mut store, &module, &imports)?;

    let doom_funcs = DoomFunctions {
        main: instance
            .exports
            .get_typed_function::<(i32, i32), i32>(&store, "main")
            .context("Failed to get main function")?,
        step: instance
            .exports
            .get_typed_function::<(), ()>(&store, "doom_loop_step")
            .context("Failed to get step function")?,
        add_event: instance
            .exports
            .get_typed_function::<(i32, i32), ()>(&store, "add_browser_event")
            .context("Failed to get add event function")?,
    };

    let mut global_state = DoomGlobalState {
        store: &mut store,
        env: &mut env,
        functions: doom_funcs,
    };

    let app_result = global_state.run();

    ratatui::restore();

    app_result
}

impl<'a> DoomGlobalState<'a> {
    fn run(&mut self) -> Result<()> {
        self.functions
            .main
            .call(self.store, 0, 0)
            .context("Failed to call main function")?;

        while !self.env.as_ref(self.store).exit {
            // Poll input events, possibly updating the TUI's state
            self.poll_events().context("failed to poll events")?;

            // Now call the step function. This does nothing if the
            // current tick isn't over.
            self.functions
                .step
                .call(self.store)
                .context("Failed to call step function")?;

            // Sleep for 1ms. No harm in a few extra calls to step,
            // but this should help keep everything more smooth, as
            // we'll always step within 1ms of the actual tick time.
            thread::sleep(Duration::from_millis(1));
        }
        Ok(())
    }

    fn poll_events(&mut self) -> Result<()> {
        while event::poll(Duration::ZERO)? {
            if let Event::Key(key) = event::read()? {
                let app = self.env.as_mut(self.store);

                match key.code {
                    // We look for a few special keys, used to control the app's
                    // behavior.
                    KeyCode::Char('q') | KeyCode::Char('Q') => {
                        if key.kind == KeyEventKind::Press {
                            app.exit();
                        }
                    }

                    KeyCode::Char('p') | KeyCode::Char('P') => {
                        if key.kind == KeyEventKind::Press {
                            app.cycle_protocol_type();
                        }
                    }

                    KeyCode::Char('+') => {
                        if key.kind == KeyEventKind::Press {
                            app.increment_zoom();
                        }
                    }

                    KeyCode::Char('-') => {
                        if key.kind == KeyEventKind::Press {
                            app.decrement_zoom();
                        }
                    }

                    // All other keys go to doom, subject to mapping rules in
                    // `key_code_to_doom_key`.
                    _ => {
                        if let (Some(code), Some(event)) = (
                            key_code_to_doom_key(key.code),
                            key_event_to_doom_event(key.kind),
                        ) {
                            self.functions
                                .add_event
                                .call(self.store, event, code)
                                .context("Failed to register input")?;
                        }
                    }
                }
            }
        }

        Ok(())
    }
}

impl DoomApp {
    fn exit(&mut self) {
        self.exit = true;
    }

    fn cycle_protocol_type(&mut self) {
        self.image_picker
            .set_protocol_type(self.image_picker.protocol_type().next());
    }

    fn set_zoom(&mut self, zoom: u16) {
        let protocol_type = self.image_picker.protocol_type();
        let mut new_picker = ratatui_image::picker::Picker::from_fontsize((
            self.default_font_size.0 / zoom,
            self.default_font_size.1 / zoom,
        ));
        new_picker.set_protocol_type(protocol_type);
        self.image_picker = new_picker;
        // No need to recreate the image, display will be updated next frame anyway
    }

    fn increment_zoom(&mut self) {
        self.set_zoom(self.zoom.saturating_add(1));
    }

    fn decrement_zoom(&mut self) {
        self.set_zoom(self.zoom.saturating_sub(1).max(1));
    }
}

fn log_string(mut env: FunctionEnvMut<DoomApp>, offset: i32, length: i32, error: bool) {
    let view = env.data().memory.view(&env);
    let slice = WasmSlice::new(&view, offset as u64, length as u64).unwrap();
    let vec = slice.read_to_vec().unwrap();
    // Doom itself presumably only outputs ASCII, and the rust wrapper
    // outputs UTF-8, so it's relatively safe to unwrap here
    let app = env.data_mut();
    app.last_log_line = Some(String::from_utf8(vec).unwrap());
    app.last_log_error = error;
}

fn log_string_normal(env: FunctionEnvMut<DoomApp>, offset: i32, length: i32) {
    log_string(env, offset, length, false);
}

fn log_string_error(env: FunctionEnvMut<DoomApp>, offset: i32, length: i32) {
    log_string(env, offset, length, true);
}

fn milliseconds_since_start(env: FunctionEnvMut<DoomApp>) -> i32 {
    env.data().started_at.elapsed().as_millis() as i32
}

fn draw_screen(mut env: FunctionEnvMut<DoomApp>, offset: i32) {
    let view = env.data().memory.view(&env);
    let slice = WasmSlice::new(&view, offset as u64, 640 * 400 * 4).unwrap();
    let image_data = slice.read_to_vec().unwrap();

    let app = env.data_mut();
    let dynamic_image =
        image::DynamicImage::ImageRgba8(image::RgbaImage::from_raw(640, 400, image_data).unwrap());
    app.current_frame = Some(
        app.image_picker
            .new_protocol(
                dynamic_image,
                Rect::new(0, 0, 640, 400),
                ratatui_image::Resize::Fit(None),
            )
            .unwrap(),
    );

    const ONE_SECOND: Duration = Duration::from_secs(1);
    if app.last_second.elapsed() < ONE_SECOND {
        app.frames_since_last_second += 1;
    } else {
        let mut seconds = 0;
        // In the odd case that we jumped more than one second since the last frame...
        while app.last_second.elapsed() >= ONE_SECOND {
            app.last_second = app.last_second.checked_add(ONE_SECOND).unwrap();
            seconds += 1
        }
        app.fps = app.frames_since_last_second / seconds;
        app.frames_since_last_second = 0;
    }

    TERMINAL
        .with(|t| {
            t.borrow_mut()
                .as_mut()
                .unwrap()
                .draw(|frame| frame.render_widget(&*app, frame.area()))
                // Ignore the result since we can't return it due to
                // lifetime issues, and we don't need it anyway
                .map(|_| ())
        })
        .unwrap();
}

impl Widget for &DoomApp {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let title = Line::from(vec![
            " WASM DooM in TUI - FPS: ".bold(),
            self.fps.to_string().bold(),
            " - Protocol: ".bold(),
            format!("{:?}", self.image_picker.protocol_type()).bold(),
            " ".bold(),
        ]);
        let instructions = Line::from(vec![
            " Quit ".into(),
            "<Q>".blue().bold(),
            " - Switch Image Protocol ".into(),
            "<P>".blue().bold(),
            " - Increase Zoom ".into(),
            "<+>".blue().bold(),
            " - Decrease Zoom ".into(),
            "<-> ".blue().bold(),
        ]);
        let block = Block::bordered()
            .title(title.centered())
            .title_bottom(instructions.centered())
            .border_set(border::THICK);

        let log_text = self.last_log_line.as_deref().unwrap_or("").to_string();

        let log_text = if self.last_log_error {
            log_text.red()
        } else {
            log_text.yellow()
        };

        let log_text = Text::from(log_text);

        Paragraph::new(log_text)
            .centered()
            .block(block)
            .render(area, buf);

        // I'm not that good with ratatui, let's just do some manual math and
        // draw over the empty part of the block
        let image = ratatui_image::Image::new(self.current_frame.as_ref().unwrap());
        image.render(Rect::new(2, 2, area.width - 4, area.height - 3), buf);
    }
}

fn key_event_to_doom_event(key_event: KeyEventKind) -> Option<i32> {
    match key_event {
        KeyEventKind::Press => Some(0),
        KeyEventKind::Release => Some(1),
        KeyEventKind::Repeat => None,
    }
}

// var keys = { KEY_ESCAPE: 27, KEY_TAB: 9 }
fn key_code_to_doom_key(key_code: KeyCode) -> Option<i32> {
    match key_code {
        KeyCode::Enter => Some(13),
        KeyCode::Backspace => Some(127),
        KeyCode::Char(' ') => Some(32),
        KeyCode::Left => Some(0xac),
        KeyCode::Right => Some(0xae),
        KeyCode::Up => Some(0xad),
        KeyCode::Down => Some(0xaf),
        KeyCode::Tab => Some(9),
        KeyCode::Esc => Some(27),

        // Since reading individual modifiers isn't globally supported, we map
        // z, x, c, v to ctrl, shift, alt, space in that order. Space is mapped
        // so you can just use four nearby keys and it doesn't get too awkward.
        KeyCode::Char('z') => Some(0x80 + 0x1d), // ctrl
        KeyCode::Char('x') => Some(0x80 + 0x38), // alt
        KeyCode::Char('c') => Some(16),          // shift
        KeyCode::Char('v') => Some(32),          // space, also mapped above

        KeyCode::Char(ch) => Some(ch as i32),
        KeyCode::F(f) => Some(f as i32 + 187),

        _ => None,
    }
}
