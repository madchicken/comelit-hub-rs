use anyhow::Result;
use comelit_hub_rs::{
    ComelitClient, ComelitClientError, ComelitOptions, DeviceStatus, get_secrets,
};
use ratatui::{
    DefaultTerminal,
    buffer::Buffer,
    crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind},
    layout::{Constraint, Layout, Rect},
    style::{
        Color, Modifier, Style, Stylize,
        palette::tailwind::{BLUE, GREEN, SLATE},
    },
    symbols,
    text::Line,
    widgets::{
        Block, Borders, HighlightSpacing, List, ListItem, ListState, Padding, Paragraph,
        StatefulWidget, Widget, Wrap,
    },
};

const TODO_HEADER_STYLE: Style = Style::new().fg(SLATE.c100).bg(BLUE.c800);
const NORMAL_ROW_BG: Color = SLATE.c950;
const ALT_ROW_BG_COLOR: Color = SLATE.c900;
const SELECTED_STYLE: Style = Style::new().bg(SLATE.c800).add_modifier(Modifier::BOLD);
const TEXT_FG_COLOR: Color = SLATE.c200;
const COMPLETED_TEXT_FG_COLOR: Color = GREEN.c500;

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install().expect("Failed to install Color Eyre");
    let terminal = ratatui::init();
    let app_result = App::new(
        "admin",
        "admin",
        Some("192.168.0.66".to_string()),
        Some(1883),
    )
    .await?;

    app_result.run(terminal)?;
    ratatui::restore();
    Ok(())
}

struct App {
    should_exit: bool,
    accessory_list: AccessoryList,
    client: ComelitClient,
}

#[derive(Default)]
struct AccessoryList {
    items: Vec<AccessoryItem>,
    state: ListState,
}

#[derive(Debug, Default)]
struct AccessoryItem {
    id: String,
    description: String,
    status: DeviceStatus,
}

impl App {
    fn run(mut self, mut terminal: DefaultTerminal) -> Result<()> {
        while !self.should_exit {
            terminal.draw(|frame| frame.render_widget(&mut self, frame.area()))?;
            if let Event::Key(key) = event::read()? {
                self.handle_key(key);
            };
        }
        Ok(())
    }

    fn handle_key(&mut self, key: KeyEvent) {
        if key.kind != KeyEventKind::Press {
            return;
        }
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.should_exit = true,
            KeyCode::Char('h') | KeyCode::Left => self.select_none(),
            KeyCode::Char('j') | KeyCode::Down => self.select_next(),
            KeyCode::Char('k') | KeyCode::Up => self.select_previous(),
            KeyCode::Char('g') | KeyCode::Home => self.select_first(),
            KeyCode::Char('G') | KeyCode::End => self.select_last(),
            KeyCode::Char('l') | KeyCode::Right | KeyCode::Enter => {
                self.toggle_status();
            }
            _ => {}
        }
    }

    fn select_none(&mut self) {
        self.accessory_list.state.select(None);
    }

    fn select_next(&mut self) {
        self.accessory_list.state.select_next();
    }
    fn select_previous(&mut self) {
        self.accessory_list.state.select_previous();
    }

    fn select_first(&mut self) {
        self.accessory_list.state.select_first();
    }

    fn select_last(&mut self) {
        self.accessory_list.state.select_last();
    }

    /// Changes the status of the selected list item
    fn toggle_status(&mut self) {
        if let Some(i) = self.accessory_list.state.selected() {
            self.accessory_list.items[i].status = match self.accessory_list.items[i].status {
                DeviceStatus::On => DeviceStatus::Off,
                DeviceStatus::Off => DeviceStatus::On,
                DeviceStatus::Running => DeviceStatus::Running,
            }
        }
    }

    async fn new(
        user: &str,
        password: &str,
        host: Option<String>,
        port: Option<u16>,
    ) -> Result<Self> {
        let (mqtt_user, mqtt_password) = get_secrets();
        let options = ComelitOptions::builder()
            .user(Some(user.into()))
            .password(Some(password.into()))
            .mqtt_user(mqtt_user)
            .mqtt_password(mqtt_password)
            .host(host)
            .port(port)
            .build()
            .map_err(|e| ComelitClientError::Generic(e.to_string()))?;
        let client = ComelitClient::new(options, None).await?;

        Ok(Self {
            should_exit: false,
            accessory_list: AccessoryList::default(),
            client,
        })
    }
}

impl Widget for &mut App {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let [header_area, main_area, footer_area] = Layout::vertical([
            Constraint::Length(2),
            Constraint::Fill(1),
            Constraint::Length(1),
        ])
        .areas(area);

        let [list_area, item_area] =
            Layout::vertical([Constraint::Fill(1), Constraint::Fill(1)]).areas(main_area);

        App::render_header(header_area, buf);
        App::render_footer(footer_area, buf);
        self.render_list(list_area, buf);
        self.render_selected_item(item_area, buf);
    }
}

/// Rendering logic for the app
impl App {
    fn render_header(area: Rect, buf: &mut Buffer) {
        Paragraph::new("Ratatui List Example")
            .bold()
            .centered()
            .render(area, buf);
    }

    fn render_footer(area: Rect, buf: &mut Buffer) {
        Paragraph::new("Use ↓↑ to move, ← to unselect, → to change status, g/G to go top/bottom.")
            .centered()
            .render(area, buf);
    }

    fn render_list(&mut self, area: Rect, buf: &mut Buffer) {
        let block = Block::new()
            .title(Line::raw("TODO List").centered())
            .borders(Borders::TOP)
            .border_set(symbols::border::EMPTY)
            .border_style(TODO_HEADER_STYLE)
            .bg(NORMAL_ROW_BG);

        // Iterate through all elements in the `items` and stylize them.
        let items: Vec<ListItem> = self
            .accessory_list
            .items
            .iter()
            .enumerate()
            .map(|(i, todo_item)| {
                let color = alternate_colors(i);
                ListItem::from(todo_item).bg(color)
            })
            .collect();

        // Create a List from all list items and highlight the currently selected one
        let list = List::new(items)
            .block(block)
            .highlight_style(SELECTED_STYLE)
            .highlight_symbol(">")
            .highlight_spacing(HighlightSpacing::Always);

        // We need to disambiguate this trait method as both `Widget` and `StatefulWidget` share the
        // same method name `render`.
        StatefulWidget::render(list, area, buf, &mut self.accessory_list.state);
    }

    fn render_selected_item(&self, area: Rect, buf: &mut Buffer) {
        // We get the info depending on the item's state.
        let info = if let Some(i) = self.accessory_list.state.selected() {
            match self.accessory_list.items[i].status {
                DeviceStatus::On => format!("✓ ON: {}", self.accessory_list.items[i].description),
                DeviceStatus::Off => format!("☐ OFF: {}", self.accessory_list.items[i].description),
                DeviceStatus::Running => {
                    format!("▶ RUNNING: {}", self.accessory_list.items[i].description)
                }
            }
        } else {
            "Nothing selected...".to_string()
        };

        // We show the list item's info under the list in this paragraph
        let block = Block::new()
            .title(Line::raw("TODO Info").centered())
            .borders(Borders::TOP)
            .border_set(symbols::border::EMPTY)
            .border_style(TODO_HEADER_STYLE)
            .bg(NORMAL_ROW_BG)
            .padding(Padding::horizontal(1));

        // We can now render the item info
        Paragraph::new(info)
            .block(block)
            .fg(TEXT_FG_COLOR)
            .wrap(Wrap { trim: false })
            .render(area, buf);
    }
}

const fn alternate_colors(i: usize) -> Color {
    if i.is_multiple_of(2) {
        NORMAL_ROW_BG
    } else {
        ALT_ROW_BG_COLOR
    }
}

impl From<&AccessoryItem> for ListItem<'_> {
    fn from(value: &AccessoryItem) -> Self {
        let line = match value.status {
            DeviceStatus::Off => Line::styled(format!(" ☐ {}", value.description), TEXT_FG_COLOR),
            DeviceStatus::On => {
                Line::styled(format!(" ✓ {}", value.description), COMPLETED_TEXT_FG_COLOR)
            }
            DeviceStatus::Running => {
                Line::styled(format!("▶ {}", value.description), COMPLETED_TEXT_FG_COLOR)
            }
        };
        ListItem::new(line)
    }
}
