//! Interactive terminal UI: tabbed feeds, compose, reply, open and delete.
//!
//! Every API call costs credits, so nothing loads in the background: a feed
//! loads when first opened, and again only when asked (R reload, m more).

use crate::api::{self, Client, Page};
use crate::view::PostView;
use anyhow::{Result, bail};
use jiff::{Timestamp, Zoned};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, Clear, HighlightSpacing, List, ListItem, ListState, Paragraph, Tabs, Wrap,
};
use ratatui::{DefaultTerminal, Frame};
use std::process::Stdio;

/// Posts fetched per load. Each one is a paid read, so keep it modest.
const PAGE_SIZE: u32 = 10;
const MAX_POST_CHARS: usize = 280;
const HINTS: &str = "c post · r reply · o open · R reload · m more · ? help";

pub fn run() -> Result<()> {
    // Load credentials before taking over the terminal so errors print normally.
    let client = Client::load()?;
    let mut terminal = ratatui::init();
    let result = App::new(client).run(&mut terminal);
    ratatui::restore();
    result
}

#[derive(Clone, PartialEq)]
enum Feed {
    Home,
    Mentions,
    User { id: String, username: String },
}

impl Feed {
    fn title(&self) -> String {
        match self {
            Feed::Home => "Home".into(),
            Feed::Mentions => "Mentions".into(),
            Feed::User { username, .. } => format!("@{username}"),
        }
    }
}

struct Tab {
    feed: Feed,
    posts: Vec<PostView>,
    list: ListState,
    next_token: Option<String>,
    loaded: bool,
}

impl Tab {
    fn new(feed: Feed) -> Self {
        Self {
            feed,
            posts: Vec::new(),
            list: ListState::default(),
            next_token: None,
            loaded: false,
        }
    }

    fn selected(&self) -> Option<&PostView> {
        self.list.selected().and_then(|i| self.posts.get(i))
    }
}

enum Mode {
    Browse,
    Compose {
        reply_to: Option<PostView>,
        text: String,
    },
    FindUser {
        text: String,
    },
    ConfirmDelete,
    Help,
}

/// Everything on screen, kept apart from the API client so it renders in tests.
struct Ui {
    me_id: String,
    me: String,
    tabs: Vec<Tab>,
    active: usize,
    mode: Mode,
    status: String,
    spent: f64,
}

impl Ui {
    fn new(me_id: String, me: String) -> Self {
        Self {
            me_id,
            me,
            tabs: vec![Tab::new(Feed::Home), Tab::new(Feed::Mentions)],
            active: 0,
            mode: Mode::Browse,
            status: String::new(),
            spent: 0.0,
        }
    }

    fn tab(&self) -> &Tab {
        &self.tabs[self.active]
    }

    fn tab_mut(&mut self) -> &mut Tab {
        &mut self.tabs[self.active]
    }

    fn is_mine(&self, post: &PostView) -> bool {
        // Undoing a repost needs a different endpoint, so only originals are deletable.
        post.author_id.as_deref() == Some(self.me_id.as_str()) && !post.is_repost
    }

    fn error(&mut self, error: anyhow::Error) {
        self.status = format!("Error: {error:#}").replace('\n', " ");
    }

    fn add_page(&mut self, page: Page, more: bool) {
        let cost = page.read_cost();
        self.spent += cost;
        let views = PostView::all(&page);
        let count = views.len();
        let tab = self.tab_mut();
        if more {
            // Jump to the first newly loaded post.
            let first_new = tab.posts.len();
            tab.posts.extend(views);
            if count > 0 {
                tab.list.select(Some(first_new));
            }
        } else {
            tab.posts = views;
            tab.list.select((count > 0).then_some(0));
        }
        tab.next_token = page.meta.next_token;
        tab.loaded = true;
        self.status = format!("Loaded {count} posts (≈${cost:.3})");
    }

    fn render(&mut self, frame: &mut Frame) {
        let [header, body, footer] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .areas(frame.area());
        self.render_header(frame, header);
        self.render_posts(frame, body);
        self.render_footer(frame, footer);

        match &self.mode {
            Mode::Browse => {}
            Mode::Compose { reply_to, text } => render_compose(frame, reply_to.as_ref(), text),
            Mode::FindUser { text } => render_find_user(frame, text),
            Mode::ConfirmDelete => render_confirm_delete(frame),
            Mode::Help => render_help(frame),
        }
    }

    fn render_header(&self, frame: &mut Frame, area: Rect) {
        let me = format!("@{} ", self.me);
        let [tabs_area, me_area] =
            Layout::horizontal([Constraint::Min(0), Constraint::Length(me.len() as u16)])
                .areas(area);
        let titles = self
            .tabs
            .iter()
            .enumerate()
            .map(|(i, tab)| format!("{} {}", i + 1, tab.feed.title()));
        let tabs = Tabs::new(titles)
            .select(self.active)
            .highlight_style(Style::new().bold().cyan().reversed())
            .divider("│");
        frame.render_widget(tabs, tabs_area);
        frame.render_widget(Line::from(me).dim().right_aligned(), me_area);
    }

    fn render_posts(&mut self, frame: &mut Frame, area: Rect) {
        let tab = &self.tabs[self.active];
        if tab.posts.is_empty() {
            let message = if tab.loaded {
                "No posts."
            } else {
                "Not loaded yet. Press R to load."
            };
            let area = Rect {
                y: area.y + area.height / 2,
                height: 1,
                ..area
            };
            frame.render_widget(Line::from(message).dim().centered(), area);
            return;
        }

        // Room for the "▌ " highlight column and a right margin.
        let width = (area.width as usize).saturating_sub(3).max(10);
        let now = Zoned::now();
        let items: Vec<ListItem> = tab
            .posts
            .iter()
            .map(|post| post_item(post, &now, width))
            .collect();
        let list = List::new(items)
            .highlight_symbol("▌ ")
            .repeat_highlight_symbol(true)
            .highlight_spacing(HighlightSpacing::Always)
            .highlight_style(Style::new().yellow());
        frame.render_stateful_widget(list, area, &mut self.tabs[self.active].list);
    }

    fn render_footer(&self, frame: &mut Frame, area: Rect) {
        let spent = format!(" ≈${:.3} this session", self.spent);
        let [left, right] =
            Layout::horizontal([Constraint::Min(0), Constraint::Length(spent.len() as u16)])
                .areas(area);
        let status = if self.status.is_empty() {
            Line::from(HINTS).dim()
        } else {
            Line::from(self.status.as_str())
        };
        frame.render_widget(status, left);
        frame.render_widget(Line::from(spent).dim().right_aligned(), right);
    }
}

fn post_item<'a>(post: &PostView, now: &Zoned, width: usize) -> ListItem<'a> {
    let mut header = vec![Span::from(format!("@{}", post.username)).bold().cyan()];
    if !post.name.is_empty() {
        header.push(Span::from(format!(" {}", post.name)));
    }
    header.push(Span::from(format!(" · {}", post.when(now))).dim());
    if let Some(by) = &post.reposted_by {
        header.push(Span::from(format!(" · ⟲ @{by}")).green());
    }

    let mut lines = vec![Line::from(header)];
    lines.extend(
        textwrap::wrap(&post.text, width)
            .into_iter()
            .map(|line| Line::from(line.into_owned())),
    );
    lines.push(Line::default());
    ListItem::new(lines)
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}

fn popup(frame: &mut Frame, width: u16, height: u16) -> Rect {
    let area = centered(frame.area(), width, height);
    frame.render_widget(Clear, area);
    area
}

fn render_compose(frame: &mut Frame, reply_to: Option<&PostView>, text: &str) {
    let area = popup(frame, 72, 12);
    let title = match reply_to {
        Some(post) => format!(" Reply to @{} ", post.username),
        None => " New post ".into(),
    };

    let count = text.chars().count();
    let mut footer = vec![Span::from(format!(" {count}/{MAX_POST_CHARS}"))];
    if count > MAX_POST_CHARS {
        footer[0] = footer[0].clone().red().bold();
    }
    if api::post_cost(text) > api::post_cost("") {
        footer.push(Span::from(" · link ≈$0.20").yellow());
    }
    footer.push(Span::from(" · Enter send · Alt+Enter newline · Esc cancel ").dim());

    let mut lines = Vec::new();
    if let Some(post) = reply_to {
        let first_line = post.text.lines().next().unwrap_or_default();
        lines.push(Line::from(format!("↳ {first_line}")).dim());
        lines.push(Line::default());
    }
    // split('\n') rather than lines() so a trailing newline moves the cursor down.
    lines.extend(text.split('\n').map(|line| Line::from(line.to_string())));
    if let Some(last) = lines.last_mut() {
        last.push_span(Span::from(" ").reversed());
    }

    let paragraph = Paragraph::new(Text::from(lines))
        .wrap(Wrap { trim: false })
        .block(
            Block::bordered()
                .title(title.bold())
                .title_bottom(Line::from(footer)),
        );
    frame.render_widget(paragraph, area);
}

fn render_find_user(frame: &mut Frame, text: &str) {
    let area = popup(frame, 48, 3);
    let line = Line::from(vec![
        Span::from("@").dim(),
        Span::from(text.to_string()),
        Span::from(" ").reversed(),
    ]);
    let block = Block::bordered()
        .title(" Open user ".bold())
        .title_bottom(Line::from(" Enter open (≈$0.01) · Esc cancel ").dim());
    frame.render_widget(Paragraph::new(line).block(block), area);
}

fn render_confirm_delete(frame: &mut Frame) {
    let area = popup(frame, 36, 3);
    let block = Block::bordered().title(" Delete post ".bold());
    let line = Line::from(vec![
        Span::from("Delete this post? "),
        Span::from("y").bold().red(),
        Span::from("/n"),
    ])
    .centered();
    frame.render_widget(Paragraph::new(line).block(block), area);
}

fn render_help(frame: &mut Frame) {
    const KEYS: [(&str, &str); 14] = [
        ("j / k  ↓ / ↑", "move between posts"),
        ("g / G", "first / last post"),
        ("PgDn / PgUp", "jump 5 posts"),
        ("tab / 1-9", "switch feed"),
        ("R", "reload feed (paid read)"),
        ("m", "load older posts (paid read)"),
        ("c", "write a new post"),
        ("r", "reply to selected post"),
        ("o / Enter", "open post in browser"),
        ("d", "delete your own post"),
        ("u", "open a user's posts"),
        ("x", "close user tab"),
        ("?", "toggle this help"),
        ("q / Ctrl+C", "quit"),
    ];
    let area = popup(frame, 52, KEYS.len() as u16 + 2);
    let lines: Vec<Line> = KEYS
        .iter()
        .map(|(key, action)| {
            Line::from(vec![
                Span::from(format!(" {key:<15}")).bold().cyan(),
                Span::from(*action),
            ])
        })
        .collect();
    let block = Block::bordered().title(" Keys ".bold());
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

/// Opens a URL without letting the browser's output scribble over the TUI.
fn open_in_browser(url: &str) -> Result<()> {
    for mut command in open::commands(url) {
        let spawned = command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        if spawned.is_ok() {
            return Ok(());
        }
    }
    bail!("could not open a browser")
}

struct App {
    client: Client,
    ui: Ui,
    quit: bool,
}

impl App {
    fn new(client: Client) -> Self {
        let ui = Ui::new(
            client.session.user_id.clone(),
            client.session.username.clone(),
        );
        Self {
            client,
            ui,
            quit: false,
        }
    }

    fn run(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        self.load(terminal, false)?;
        while !self.quit {
            terminal.draw(|frame| self.ui.render(frame))?;
            if let Event::Key(key) = event::read()?
                && key.kind == KeyEventKind::Press
            {
                self.on_key(key, terminal)?;
            }
        }
        Ok(())
    }

    /// Shows `message` while a blocking API call runs.
    fn busy(&mut self, terminal: &mut DefaultTerminal, message: &str) -> Result<()> {
        self.ui.status = message.into();
        terminal.draw(|frame| self.ui.render(frame))?;
        Ok(())
    }

    fn on_key(&mut self, key: KeyEvent, terminal: &mut DefaultTerminal) -> Result<()> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && key.code == KeyCode::Char('c') {
            self.quit = true;
            return Ok(());
        }

        // Take the mode out; each arm puts back whatever mode should follow.
        match std::mem::replace(&mut self.ui.mode, Mode::Browse) {
            Mode::Browse => self.on_browse_key(key, terminal)?,
            Mode::Help => {}
            Mode::ConfirmDelete => {
                if key.code == KeyCode::Char('y') {
                    self.delete_selected(terminal)?;
                } else {
                    self.ui.status = "Kept.".into();
                }
            }
            Mode::Compose { reply_to, mut text } => match key.code {
                KeyCode::Esc => self.ui.status = "Discarded.".into(),
                KeyCode::Enter
                    if key
                        .modifiers
                        .intersects(KeyModifiers::ALT | KeyModifiers::SHIFT) =>
                {
                    text.push('\n');
                    self.ui.mode = Mode::Compose { reply_to, text };
                }
                KeyCode::Enter => self.send_post(terminal, reply_to, text)?,
                code => {
                    match code {
                        KeyCode::Backspace => {
                            text.pop();
                        }
                        KeyCode::Char(c) if !ctrl => text.push(c),
                        _ => {}
                    }
                    self.ui.mode = Mode::Compose { reply_to, text };
                }
            },
            Mode::FindUser { mut text } => match key.code {
                KeyCode::Esc => {}
                KeyCode::Enter => self.open_user(terminal, &text)?,
                code => {
                    match code {
                        KeyCode::Backspace => {
                            text.pop();
                        }
                        // The popup already shows the "@" prefix.
                        KeyCode::Char(c) if !ctrl && !c.is_whitespace() && c != '@' => text.push(c),
                        _ => {}
                    }
                    self.ui.mode = Mode::FindUser { text };
                }
            },
        }
        Ok(())
    }

    fn on_browse_key(&mut self, key: KeyEvent, terminal: &mut DefaultTerminal) -> Result<()> {
        self.ui.status.clear();
        let tabs = self.ui.tabs.len();
        match key.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Char('j') | KeyCode::Down => self.ui.tab_mut().list.select_next(),
            KeyCode::Char('k') | KeyCode::Up => self.ui.tab_mut().list.select_previous(),
            KeyCode::Char('g') | KeyCode::Home => self.ui.tab_mut().list.select_first(),
            KeyCode::Char('G') | KeyCode::End => self.ui.tab_mut().list.select_last(),
            KeyCode::PageDown => self.ui.tab_mut().list.scroll_down_by(5),
            KeyCode::PageUp => self.ui.tab_mut().list.scroll_up_by(5),
            KeyCode::Tab | KeyCode::Char('l') | KeyCode::Right => {
                self.switch_tab(terminal, (self.ui.active + 1) % tabs)?
            }
            KeyCode::BackTab | KeyCode::Char('h') | KeyCode::Left => {
                self.switch_tab(terminal, (self.ui.active + tabs - 1) % tabs)?
            }
            KeyCode::Char(c @ '1'..='9') => {
                let index = c as usize - '1' as usize;
                if index < tabs {
                    self.switch_tab(terminal, index)?;
                }
            }
            KeyCode::Char('R') | KeyCode::F(5) => self.load(terminal, false)?,
            KeyCode::Char('m') => self.load(terminal, true)?,
            KeyCode::Char('c') => {
                self.ui.mode = Mode::Compose {
                    reply_to: None,
                    text: String::new(),
                }
            }
            KeyCode::Char('r') => match self.ui.tab().selected().cloned() {
                Some(post) => {
                    self.ui.mode = Mode::Compose {
                        reply_to: Some(post),
                        text: String::new(),
                    }
                }
                None => self.ui.status = "No post selected.".into(),
            },
            KeyCode::Char('o') | KeyCode::Enter => {
                if let Some(url) = self.ui.tab().selected().map(PostView::url) {
                    match open_in_browser(&url) {
                        Ok(()) => self.ui.status = format!("Opened {url}"),
                        Err(e) => self.ui.error(e),
                    }
                }
            }
            KeyCode::Char('d') => match self.ui.tab().selected() {
                Some(post) if self.ui.is_mine(post) => self.ui.mode = Mode::ConfirmDelete,
                Some(_) => self.ui.status = "You can only delete your own posts.".into(),
                None => {}
            },
            KeyCode::Char('u') | KeyCode::Char('/') => {
                self.ui.mode = Mode::FindUser {
                    text: String::new(),
                }
            }
            KeyCode::Char('x') => {
                if matches!(self.ui.tab().feed, Feed::User { .. }) {
                    self.ui.tabs.remove(self.ui.active);
                    self.ui.active -= 1;
                }
            }
            KeyCode::Char('?') => self.ui.mode = Mode::Help,
            _ => {}
        }
        Ok(())
    }

    fn switch_tab(&mut self, terminal: &mut DefaultTerminal, index: usize) -> Result<()> {
        self.ui.active = index;
        if !self.ui.tab().loaded {
            self.load(terminal, false)?;
        }
        Ok(())
    }

    /// Fetches the active feed: fresh from the top, or the next older page.
    fn load(&mut self, terminal: &mut DefaultTerminal, more: bool) -> Result<()> {
        let tab = self.ui.tab();
        let feed = tab.feed.clone();
        let token = match (more, tab.next_token.clone()) {
            (false, _) => None,
            (true, Some(token)) => Some(token),
            (true, None) => {
                self.ui.status = "No older posts.".into();
                return Ok(());
            }
        };

        self.busy(terminal, &format!("Loading {}…", feed.title()))?;
        let token = token.as_deref();
        let result = match &feed {
            Feed::Home => self.client.home(PAGE_SIZE, token),
            Feed::Mentions => self.client.mentions(PAGE_SIZE, token),
            Feed::User { id, .. } => self.client.user_posts(id, PAGE_SIZE, token),
        };
        match result {
            Ok(page) => self.ui.add_page(page, more),
            Err(e) => self.ui.error(e),
        }
        Ok(())
    }

    fn send_post(
        &mut self,
        terminal: &mut DefaultTerminal,
        reply_to: Option<PostView>,
        text: String,
    ) -> Result<()> {
        let body = text.trim();
        if body.is_empty() {
            self.ui.mode = Mode::Compose { reply_to, text };
            return Ok(());
        }

        self.busy(terminal, "Posting…")?;
        match self
            .client
            .post(body, reply_to.as_ref().map(|p| p.id.as_str()))
        {
            Ok(created) => {
                self.ui.spent += api::post_cost(body);
                let session = &self.client.session;
                let mine = PostView {
                    id: created.id,
                    author_id: Some(session.user_id.clone()),
                    username: session.username.clone(),
                    name: String::new(),
                    created_at: Some(Timestamp::now().to_string()),
                    reposted_by: None,
                    is_repost: false,
                    text: body.to_string(),
                };
                self.ui.status = format!("Posted {}", mine.url());
                // Show a new post at the top of Home without paying to reload it.
                if reply_to.is_none()
                    && let Some(home) = self.ui.tabs.iter_mut().find(|t| t.feed == Feed::Home)
                {
                    home.posts.insert(0, mine);
                    home.list.select(Some(0));
                }
            }
            Err(e) => {
                self.ui.error(e);
                // Keep the draft so nothing typed is lost.
                self.ui.mode = Mode::Compose { reply_to, text };
            }
        }
        Ok(())
    }

    fn delete_selected(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        let tab = self.ui.tab();
        let Some((index, id)) = tab
            .list
            .selected()
            .and_then(|i| tab.posts.get(i).map(|post| (i, post.id.clone())))
        else {
            return Ok(());
        };

        self.busy(terminal, "Deleting…")?;
        match self.client.delete(&id) {
            Ok(()) => {
                let tab = self.ui.tab_mut();
                tab.posts.remove(index);
                let last = tab.posts.len().checked_sub(1);
                tab.list.select(last.map(|last| index.min(last)));
                self.ui.status = "Deleted.".into();
            }
            Err(e) => self.ui.error(e),
        }
        Ok(())
    }

    fn open_user(&mut self, terminal: &mut DefaultTerminal, text: &str) -> Result<()> {
        let username = text.trim().trim_start_matches('@');
        if username.is_empty() {
            return Ok(());
        }
        let existing = self.ui.tabs.iter().position(|tab| {
            matches!(&tab.feed, Feed::User { username: u, .. } if u.eq_ignore_ascii_case(username))
        });
        if let Some(index) = existing {
            return self.switch_tab(terminal, index);
        }

        self.busy(terminal, &format!("Looking up @{username}…"))?;
        match self.client.lookup_user(username) {
            Ok(user) => {
                self.ui.spent += api::COST_READ_USER;
                self.ui.tabs.push(Tab::new(Feed::User {
                    id: user.id,
                    username: user.username,
                }));
                self.switch_tab(terminal, self.ui.tabs.len() - 1)?;
            }
            Err(e) => self.ui.error(e),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::tests::repost_page;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn screen(ui: &mut Ui, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| ui.render(frame)).unwrap();
        let buffer = terminal.backend().buffer();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn loaded_ui() -> Ui {
        let mut ui = Ui::new("10".into(), "yoda".into());
        ui.add_page(repost_page(), false);
        ui
    }

    #[test]
    fn renders_feed_with_reposts_and_cost() {
        let mut ui = loaded_ui();
        let screen = screen(&mut ui, 80, 16);
        println!("{screen}");
        assert!(screen.contains("1 Home"));
        assert!(screen.contains("2 Mentions"));
        assert!(screen.contains("@yoda"));
        assert!(screen.contains("▌ @yunta Yunta ·"));
        assert!(screen.contains("⟲ @yoda"));
        assert!(screen.contains("it can do everything."));
        assert!(screen.contains("deleted original & more"));
        assert!(screen.contains("Loaded 2 posts (≈$0.015)"));
        assert!(screen.contains("≈$0.015 this session"));
    }

    #[test]
    fn unloaded_feed_explains_how_to_load() {
        let mut ui = Ui::new("10".into(), "yoda".into());
        assert!(screen(&mut ui, 60, 10).contains("Press R to load"));
    }

    #[test]
    fn compose_popup_shows_reply_context_count_and_link_cost() {
        let mut ui = loaded_ui();
        let reply_to = ui.tab().selected().cloned();
        ui.mode = Mode::Compose {
            reply_to,
            text: "see https://example.com".into(),
        };
        let screen = screen(&mut ui, 90, 20);
        println!("{screen}");
        assert!(screen.contains("Reply to @yunta"));
        assert!(screen.contains("↳ For now the app"));
        assert!(screen.contains("23/280"));
        assert!(screen.contains("link ≈$0.20 · Enter send · Alt+Enter newline · Esc cancel"));
    }

    #[test]
    fn help_lists_keys() {
        let mut ui = loaded_ui();
        ui.mode = Mode::Help;
        let screen = screen(&mut ui, 80, 24);
        assert!(screen.contains("reload feed (paid read)"));
    }

    #[test]
    fn load_more_appends_and_selects_first_new_post() {
        let mut ui = loaded_ui();
        ui.add_page(repost_page(), true);
        assert_eq!(ui.tab().posts.len(), 4);
        assert_eq!(ui.tab().list.selected(), Some(2));
        assert!((ui.spent - 0.030).abs() < 1e-9);
    }

    #[test]
    fn only_own_original_posts_are_deletable() {
        let ui = loaded_ui();
        // A repost by me shows someone else's original: not deletable.
        assert!(!ui.is_mine(&ui.tab().posts[0]));
        // A repost whose original is missing is attributed to me, but is still a repost.
        let mut own = ui.tab().posts[1].clone();
        assert!(!ui.is_mine(&own));
        own.is_repost = false;
        assert!(ui.is_mine(&own));
        own.author_id = Some("99".into());
        assert!(!ui.is_mine(&own));
    }
}
