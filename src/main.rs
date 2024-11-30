use std::{cell::RefCell, rc::Rc, thread, time::Duration};

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

// This needs to be static so it's accessible to the rendering WASM import function.
// Since we only have one thread, we can safely use an Rc. However, Rust doesn't know
// this, so we need to make it a thread local to keep Rust happy.
thread_local! {
    static TERMINAL: Rc<RefCell<Option<DefaultTerminal>>> = Rc::new(RefCell::new(None));
}

fn main() -> Result<()> {
    let terminal = ratatui::init();
    TERMINAL.with(move |t| *t.borrow_mut() = Some(terminal));

    let app_result = App::new().run();

    ratatui::restore();

    app_result
}

struct App {
    exit: bool,
    last_key: Option<String>,
}

impl App {
    fn new() -> Self {
        Self {
            exit: false,
            last_key: None,
        }
    }

    fn run(&mut self) -> Result<()> {
        while !self.exit {
            self.poll_events().context("failed to poll events")?;
            self.render_frame().context("failed to render frame")?;
            thread::sleep(Duration::from_millis(1));
        }
        Ok(())
    }

    fn poll_events(&mut self) -> Result<()> {
        while event::poll(Duration::ZERO)? {
            match event::read()? {
                Event::Key(key)
                    if key.kind == KeyEventKind::Press
                        && (key.code == KeyCode::Char('q') || key.code == KeyCode::Char('Q')) =>
                {
                    self.exit = true;
                }
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    self.last_key = Some(key.code.to_string());
                }
                _ => (),
            }
        }

        Ok(())
    }

    fn render_frame(&self) -> Result<()> {
        TERMINAL.with(|t| {
            t.borrow_mut()
                .as_mut()
                .unwrap()
                .draw(|frame| frame.render_widget(self, frame.area()))
                // Ignore the result since we can't return it due to
                // lifetime issues, and we don't need it anyway
                .map(|_| ())
        })?;
        Ok(())
    }
}

impl Widget for &App {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let title = Line::from(" WASM DooM in TUI ".bold());
        let instructions = Line::from(vec![" Quit ".into(), "<Q> ".blue().bold()]);
        let block = Block::bordered()
            .title(title.centered())
            .title_bottom(instructions.centered())
            .border_set(border::THICK);

        let counter_text = Text::from(vec![Line::from(vec![
            "Last key: ".into(),
            self.last_key
                .as_deref()
                .unwrap_or("<None yet>")
                .to_string()
                .yellow(),
        ])]);

        Paragraph::new(counter_text)
            .centered()
            .block(block)
            .render(area, buf);
    }
}
