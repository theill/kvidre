//! Interactive terminal UI: tabbed feeds, compose, reply, open and delete.
//!
//! Every API call costs credits, so nothing loads in the background: a feed
//! loads when first opened, and again only when asked (R reload, m more).

use crate::api::{self, Client, Page, User, UserPage};
use crate::view::{PostView, reply_intent_url};
use anyhow::{Result, bail};
use jiff::{Timestamp, Zoned};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Clear, List, ListItem, ListState, Paragraph, Tabs, Wrap};
use ratatui::{DefaultTerminal, Frame};
use std::process::Stdio;

/// Posts fetched per load. Each one is a paid read, so keep it modest.
const PAGE_SIZE: u32 = 10;
/// Accounts fetched per load of the Follows tab (about $0.01 each).
const ACCOUNT_PAGE_SIZE: u32 = 100;
const MAX_POST_CHARS: usize = 280;
/// Wrapped lines of post text shown in a feed; longer posts end in "…".
const MAX_POST_LINES: usize = 10;
const HINTS: &str = "c post · r reply · o open · space expand · R reload · m more · ? help";
const FOLLOWS_HINTS: &str = "U unfollow · Enter posts · o profile · m more · ? help";

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
    /// Posts from people you follow; by default without replies and reposts.
    Following {
        originals_only: bool,
    },
    Mentions,
    User {
        id: String,
        username: String,
    },
    /// The accounts you follow (a list of people, not posts).
    Follows,
}

impl Feed {
    fn title(&self) -> String {
        match self {
            Feed::Follows => "Follows".into(),
            Feed::Following {
                originals_only: true,
            } => "Following".into(),
            Feed::Following {
                originals_only: false,
            } => "Following (all)".into(),
            Feed::Mentions => "Mentions".into(),
            Feed::User { username, .. } => format!("@{username}"),
        }
    }
}

struct Tab {
    feed: Feed,
    posts: Vec<PostView>,
    /// Filled instead of `posts` in the Follows tab.
    users: Vec<User>,
    list: ListState,
    /// ID of the selected post when Space has expanded it past MAX_POST_LINES.
    expanded: Option<String>,
    next_token: Option<String>,
    loaded: bool,
}

impl Tab {
    fn new(feed: Feed) -> Self {
        Self {
            feed,
            posts: Vec::new(),
            users: Vec::new(),
            list: ListState::default(),
            expanded: None,
            next_token: None,
            loaded: false,
        }
    }

    fn selected(&self) -> Option<&PostView> {
        self.list.selected().and_then(|i| self.posts.get(i))
    }

    fn selected_user(&self) -> Option<&User> {
        self.list.selected().and_then(|i| self.users.get(i))
    }

    fn len(&self) -> usize {
        self.posts.len() + self.users.len()
    }

    /// True when the last item is selected, so moving down should load more.
    fn at_end(&self) -> bool {
        self.list.selected().is_some_and(|i| i.saturating_add(1) >= self.len())
    }
}

/// Replaces `items` with a fresh first page, or appends an older page and
/// selects its first item.
fn merge<T>(items: &mut Vec<T>, list: &mut ListState, new: Vec<T>, more: bool) {
    let count = new.len();
    if more {
        let first_new = items.len();
        items.extend(new);
        if count > 0 {
            list.select(Some(first_new));
        }
    } else {
        *items = new;
        list.select((count > 0).then_some(0));
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
    ConfirmUnfollow,
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
            tabs: vec![
                Tab::new(Feed::Following {
                    originals_only: true,
                }),
                Tab::new(Feed::Mentions),
            ],
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
        merge(&mut tab.posts, &mut tab.list, views, more);
        tab.next_token = page.meta.next_token;
        tab.loaded = true;
        self.status = format!("Loaded {count} posts (≈${cost:.3})");
    }

    fn add_users(&mut self, page: UserPage, more: bool) {
        let cost = page.read_cost();
        self.spent += cost;
        let count = page.data.len();
        let tab = self.tab_mut();
        merge(&mut tab.users, &mut tab.list, page.data, more);
        let has_more = page.meta.next_token.is_some();
        tab.next_token = page.meta.next_token;
        tab.loaded = true;
        let total = tab.users.len();
        let more_hint = if has_more { ", m for more" } else { "" };
        self.status = format!("{total} accounts loaded (+{count}, ≈${cost:.2}){more_hint}");
    }

    fn render(&mut self, frame: &mut Frame) {
        // Heading row, a blank line of breathing room, the posts, the status bar.
        let [header, _, body, footer] = Layout::vertical([
            Constraint::Length(1),
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
            Mode::ConfirmDelete => render_confirm(frame, " Delete post ", "Delete this post?"),
            Mode::ConfirmUnfollow => {
                let who = self
                    .tab()
                    .selected_user()
                    .map_or(String::new(), |u| format!(" @{}", u.username));
                render_confirm(frame, " Unfollow ", &format!("Unfollow{who}?"))
            }
            Mode::Help => render_help(frame),
        }
    }

    fn render_header(&self, frame: &mut Frame, area: Rect) {
        // Name badge, styled like sladder's: theme cyan behind bold black.
        const BADGE: &str = " kvidre ";
        let me = format!("@{} ", self.me);
        let [badge_area, tabs_area, me_area] = Layout::horizontal([
            Constraint::Length(BADGE.len() as u16 + 1),
            Constraint::Min(0),
            Constraint::Length(me.len() as u16),
        ])
        .areas(area);
        frame.render_widget(
            Line::from(vec![
                Span::styled(BADGE, Style::new().bold().fg(Color::Black).bg(Color::Cyan)),
                // Tabs pad their titles with a space, so one more makes a two-space gap.
                Span::raw(" "),
            ]),
            badge_area,
        );
        let titles = self
            .tabs
            .iter()
            .enumerate()
            .map(|(i, tab)| format!("{} {}", i + 1, tab.feed.title()));
        let tabs = Tabs::new(titles)
            .select(self.active)
            // Dark text on the accent, like sladder's current tab.
            .highlight_style(Style::new().fg(Color::Black).bg(Color::Blue))
            .divider("│");
        frame.render_widget(tabs, tabs_area);
        frame.render_widget(Line::from(me).dim().right_aligned(), me_area);
    }

    fn render_posts(&mut self, frame: &mut Frame, area: Rect) {
        let tab = &self.tabs[self.active];
        if tab.len() == 0 {
            let message = match (tab.loaded, &tab.feed) {
                (false, _) => "Not loaded yet. Press R to load.",
                (true, Feed::Follows) => "You don't follow anyone.",
                (true, _) => "No posts.",
            };
            let area = Rect {
                y: area.y + area.height / 2,
                height: 1,
                ..area
            };
            frame.render_widget(Line::from(message).dim().centered(), area);
            return;
        }

        // Room for the "▌ " selection bar and a right margin.
        let width = (area.width as usize).saturating_sub(3).max(10);
        let now = Zoned::now();
        // The list may hold an out-of-range index (e.g. after G) until it renders.
        let selected = tab.list.selected().map(|i| i.min(tab.len() - 1));
        let items: Vec<ListItem> = if matches!(tab.feed, Feed::Follows) {
            tab.users
                .iter()
                .enumerate()
                .map(|(i, user)| user_item(user, width, selected == Some(i)))
                .collect()
        } else {
            tab.posts
                .iter()
                .enumerate()
                .map(|(i, post)| {
                    let selected = selected == Some(i);
                    let expanded = selected && tab.expanded.as_ref() == Some(&post.id);
                    post_item(post, &now, width, selected, expanded)
                })
                .collect()
        };
        let list = List::new(items).highlight_style(Style::new().yellow());
        frame.render_stateful_widget(list, area, &mut self.tabs[self.active].list);
    }

    fn render_footer(&self, frame: &mut Frame, area: Rect) {
        let spent = format!(" ≈${:.3} this session", self.spent);
        let [left, right] =
            Layout::horizontal([Constraint::Min(0), Constraint::Length(spent.len() as u16)])
                .areas(area);
        let status = if self.status.is_empty() {
            let hints = match self.tab().feed {
                Feed::Follows => FOLLOWS_HINTS,
                _ => HINTS,
            };
            Line::from(hints).dim()
        } else {
            Line::from(self.status.as_str())
        };
        frame.render_widget(status, left);
        frame.render_widget(Line::from(spent).dim().right_aligned(), right);
    }
}

/// One post as a list item. The selection bar is drawn here rather than by the
/// List, so it covers the post's lines but not the blank line separating posts.
fn post_item<'a>(
    post: &PostView,
    now: &Zoned,
    width: usize,
    selected: bool,
    expanded: bool,
) -> ListItem<'a> {
    let bar = if selected {
        Span::from("▌ ").yellow()
    } else {
        Span::from("  ")
    };
    let mut header = vec![
        bar.clone(),
        Span::from(format!("@{}", post.username)).bold().cyan(),
    ];
    if !post.name.is_empty() {
        header.push(Span::from(format!(" {}", post.name)));
    }
    header.push(Span::from(format!(" · {}", post.when(now))).dim());
    if let Some(by) = &post.reposted_by {
        header.push(Span::from(format!(" · ⟲ @{by}")).green());
    }

    let mut lines = vec![Line::from(header)];
    let body = textwrap::wrap(&post.text, width);
    let shown = if expanded { body.len() } else { MAX_POST_LINES };
    let truncated = body.len() > shown;
    lines.extend(
        body.into_iter()
            .take(shown)
            .map(|line| Line::from(vec![bar.clone(), Span::from(line.into_owned())])),
    );
    if truncated && let Some(last) = lines.last_mut() {
        last.push_span(Span::from(" …").dim());
    }
    lines.push(Line::default());
    ListItem::new(lines)
}

/// One followed account: handle and name, then up to two lines of bio.
fn user_item<'a>(user: &User, width: usize, selected: bool) -> ListItem<'a> {
    let bar = if selected {
        Span::from("▌ ").yellow()
    } else {
        Span::from("  ")
    };
    let mut lines = vec![Line::from(vec![
        bar.clone(),
        Span::from(format!("@{}", user.username)).bold().cyan(),
        Span::from(format!(" {}", user.name)),
    ])];
    // Flatten line breaks so more of the bio fits in two lines.
    let bio = user
        .description
        .as_deref()
        .unwrap_or_default()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    lines.extend(
        textwrap::wrap(&bio, width)
            .into_iter()
            .filter(|line| !line.is_empty())
            .take(2)
            .map(|line| Line::from(vec![bar.clone(), Span::from(line.into_owned()).dim()])),
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

fn render_confirm(frame: &mut Frame, title: &str, question: &str) {
    let width = (question.chars().count() as u16 + 10).max(36);
    let area = popup(frame, width, 3);
    let block = Block::bordered().title(title.to_string().bold());
    let line = Line::from(vec![
        Span::from(format!("{question} ")),
        Span::from("y").bold().red(),
        Span::from("/n"),
    ])
    .centered();
    frame.render_widget(Paragraph::new(line).block(block), area);
}

fn render_help(frame: &mut Frame) {
    const KEYS: [(&str, &str); 18] = [
        ("j / k  ↓ / ↑", "move between posts"),
        ("g / G", "first / last post"),
        ("PgDn / PgUp", "jump 5 posts"),
        ("tab / 1-9", "switch feed"),
        ("R", "reload feed (paid read)"),
        ("m  (↓ at end)", "load older posts (paid read)"),
        ("f", "Following: show/hide replies & reposts"),
        ("c", "write a new post"),
        ("r", "reply to selected post"),
        ("o / Enter", "open post in browser"),
        ("space", "expand / collapse a long post"),
        ("d", "delete your own post"),
        ("u", "open a user's posts"),
        ("F", "accounts you follow (≈$0.01 each)"),
        ("U", "Follows: unfollow selected account"),
        ("x", "close user or Follows tab"),
        ("?", "toggle this help"),
        ("q / Ctrl+C", "quit"),
    ];
    let area = popup(frame, 58, KEYS.len() as u16 + 2);
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
            Mode::ConfirmUnfollow => {
                if key.code == KeyCode::Char('y') {
                    self.unfollow_selected(terminal)?;
                } else {
                    self.ui.status = "Still following.".into();
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
        let before = self.ui.tab().selected().map(|post| post.id.clone());
        match key.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Char('j') | KeyCode::Down if self.ui.tab().at_end() => {
                self.load(terminal, true)?
            }
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
            KeyCode::Char(' ') => {
                let tab = self.ui.tab_mut();
                let id = tab.selected().map(|post| post.id.clone());
                tab.expanded = if tab.expanded == id { None } else { id };
            }
            KeyCode::Char('o') | KeyCode::Enter => {
                if let Some(user) = self.ui.tab().selected_user().cloned() {
                    // In Follows, Enter reads their posts here; o opens their profile.
                    if key.code == KeyCode::Enter {
                        self.open_user_tab(terminal, user.id, user.username)?;
                    } else {
                        self.open_url(&format!("https://x.com/{}", user.username));
                    }
                } else if let Some(url) = self.ui.tab().selected().map(PostView::url) {
                    self.open_url(&url);
                }
            }
            KeyCode::Char('F') => {
                let existing = self.ui.tabs.iter().position(|t| t.feed == Feed::Follows);
                let index = existing.unwrap_or_else(|| {
                    self.ui.tabs.push(Tab::new(Feed::Follows));
                    self.ui.tabs.len() - 1
                });
                self.switch_tab(terminal, index)?;
            }
            KeyCode::Char('U') => {
                if self.ui.tab().selected_user().is_some() {
                    self.ui.mode = Mode::ConfirmUnfollow;
                } else {
                    self.ui.status = "Press F to list who you follow, then U to unfollow.".into();
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
                if matches!(self.ui.tab().feed, Feed::User { .. } | Feed::Follows) {
                    self.ui.tabs.remove(self.ui.active);
                    self.ui.active -= 1;
                }
            }
            KeyCode::Char('f') => match &mut self.ui.tab_mut().feed {
                Feed::Following { originals_only } => {
                    *originals_only = !*originals_only;
                    // The cached posts no longer match the filter.
                    self.load(terminal, false)?;
                }
                _ => {
                    self.ui.status = "f toggles replies and reposts in Following only.".into();
                }
            },
            KeyCode::Char('?') => self.ui.mode = Mode::Help,
            _ => {}
        }
        // Moving to another post collapses the expanded one.
        let tab = self.ui.tab_mut();
        if tab.selected().map(|post| &post.id) != before.as_ref() {
            tab.expanded = None;
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
                self.ui.status = "Nothing more to load.".into();
                return Ok(());
            }
        };

        self.busy(terminal, &format!("Loading {}…", feed.title()))?;
        let token = token.as_deref();
        let result = match &feed {
            Feed::Following { originals_only } => self
                .client
                .following(PAGE_SIZE, token, *originals_only)
                .map(|page| self.ui.add_page(page, more)),
            Feed::Mentions => self
                .client
                .mentions(PAGE_SIZE, token)
                .map(|page| self.ui.add_page(page, more)),
            Feed::User { id, .. } => self
                .client
                .user_posts(id, PAGE_SIZE, token)
                .map(|page| self.ui.add_page(page, more)),
            Feed::Follows => self
                .client
                .followed_accounts(ACCOUNT_PAGE_SIZE, token)
                .map(|page| self.ui.add_users(page, more)),
        };
        if let Err(e) = result {
            self.ui.error(e);
        }
        Ok(())
    }

    fn open_url(&mut self, url: &str) {
        match open_in_browser(url) {
            Ok(()) => self.ui.status = format!("Opened {url}"),
            Err(e) => self.ui.error(e),
        }
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
                // Show a new post at the top of Following without paying to reload it.
                if reply_to.is_none()
                    && let Some(following) = self
                        .ui
                        .tabs
                        .iter_mut()
                        .find(|t| matches!(t.feed, Feed::Following { .. }))
                {
                    following.posts.insert(0, mine);
                    following.list.select(Some(0));
                }
            }
            Err(e) => {
                let restricted = e
                    .downcast_ref::<api::ApiError>()
                    .is_some_and(|e| e.reply_restricted());
                if restricted
                    && let Some(post) = &reply_to
                    && open_in_browser(&reply_intent_url(&post.id, body)).is_ok()
                {
                    self.ui.status =
                        "X only takes this reply on x.com. Opened it there, press Reply to post."
                            .into();
                    return Ok(());
                }
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

    fn unfollow_selected(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        let tab = self.ui.tab();
        let Some((index, user)) = tab
            .list
            .selected()
            .and_then(|i| tab.users.get(i).map(|user| (i, user.clone())))
        else {
            return Ok(());
        };

        self.busy(terminal, &format!("Unfollowing @{}…", user.username))?;
        match self.client.unfollow(&user.id) {
            Ok(()) => {
                let tab = self.ui.tab_mut();
                tab.users.remove(index);
                let last = tab.users.len().checked_sub(1);
                tab.list.select(last.map(|last| index.min(last)));
                self.ui.status = format!("Unfollowed @{}.", user.username);
            }
            Err(e) => self.ui.error(e),
        }
        Ok(())
    }

    /// Switches to the posts tab for a known account, opening one if needed.
    fn open_user_tab(
        &mut self,
        terminal: &mut DefaultTerminal,
        id: String,
        username: String,
    ) -> Result<()> {
        let existing = self
            .ui
            .tabs
            .iter()
            .position(|tab| matches!(&tab.feed, Feed::User { id: i, .. } if *i == id));
        let index = existing.unwrap_or_else(|| {
            self.ui.tabs.push(Tab::new(Feed::User { id, username }));
            self.ui.tabs.len() - 1
        });
        self.switch_tab(terminal, index)
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
                self.open_user_tab(terminal, user.id, user.username)?;
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
        let rows: Vec<&str> = screen.lines().collect();
        assert!(rows[0].starts_with(" kvidre   1 Following"));
        assert_eq!(rows[1], "", "blank row between header and posts");
        assert!(rows[2].starts_with("▌ @yunta"));
        assert!(rows[3].starts_with("▌ For now"));
        assert_eq!(
            rows[4], "",
            "no selection bar on the blank line between posts"
        );
        assert!(rows[5].starts_with("  @yoda"));
        assert!(screen.contains("1 Following"));
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
    fn long_posts_are_cut_to_ten_lines_until_expanded() {
        let mut ui = loaded_ui();
        let text = (1..=15)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        ui.tab_mut().posts[0].text = text;
        let cut = screen(&mut ui, 80, 30);
        assert!(cut.contains("▌ line 10 …"));
        assert!(!cut.contains("line 11"));
        assert!(cut.contains("@yoda"), "next post still follows");

        ui.tab_mut().expanded = Some(ui.tab().posts[0].id.clone());
        let full = screen(&mut ui, 80, 30);
        assert!(full.contains("▌ line 15"));
        assert!(!full.contains("line 10 …"));
    }

    #[test]
    fn at_end_only_on_last_item() {
        let mut ui = loaded_ui();
        assert!(!ui.tab().at_end());
        ui.tab_mut().list.select_last();
        assert!(
            ui.tab().at_end(),
            "select_last parks past the end until render"
        );
        ui.tab_mut().list.select(Some(1));
        assert!(ui.tab().at_end());
        assert!(!Tab::new(Feed::Mentions).at_end(), "empty tab");
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
        assert!(screen.contains("Following: show/hide replies & reposts"));
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
    fn following_tab_title_shows_filter() {
        let mut ui = loaded_ui();
        assert!(screen(&mut ui, 80, 6).contains("1 Following │ 2 Mentions"));
        ui.tabs[0].feed = Feed::Following {
            originals_only: false,
        };
        assert!(screen(&mut ui, 80, 6).contains("1 Following (all) │ 2 Mentions"));
    }

    fn follows_ui() -> Ui {
        let mut ui = loaded_ui();
        ui.tabs.push(Tab::new(Feed::Follows));
        ui.active = 2;
        let page: UserPage = serde_json::from_str(
            r#"{"data": [
                {"id": "20", "username": "yunta", "name": "Yunta",
                 "description": "Building robots.\nOpinions my own."},
                {"id": "30", "username": "quiet", "name": "Quiet One"}
            ], "meta": {"next_token": "p2"}}"#,
        )
        .unwrap();
        ui.add_users(page, false);
        ui
    }

    #[test]
    fn follows_tab_lists_accounts_with_bios() {
        let mut ui = follows_ui();
        let screen = screen(&mut ui, 80, 14);
        println!("{screen}");
        let rows: Vec<&str> = screen.lines().collect();
        assert!(rows[0].contains("3 Follows"));
        assert!(rows[2].starts_with("▌ @yunta Yunta"));
        assert!(rows[3].starts_with("▌ Building robots. Opinions my own."));
        assert_eq!(rows[4], "");
        assert!(rows[5].starts_with("  @quiet Quiet One"));
        assert_eq!(rows[6], "", "no bio line for accounts without one");
        assert!(screen.contains("2 accounts loaded (+2, ≈$0.02), m for more"));
        assert!(
            !ui.tab().selected().is_some(),
            "no post is selected in Follows"
        );
    }

    #[test]
    fn follows_tab_shows_its_own_hints_and_unfollow_prompt() {
        let mut ui = follows_ui();
        ui.status.clear();
        assert!(screen(&mut ui, 80, 14).contains("U unfollow · Enter posts · o profile"));
        ui.mode = Mode::ConfirmUnfollow;
        assert!(screen(&mut ui, 80, 14).contains("Unfollow @yunta? y/n"));
    }

    #[test]
    fn loading_more_accounts_appends() {
        let mut ui = follows_ui();
        let page: UserPage =
            serde_json::from_str(r#"{"data": [{"id": "40", "username": "late", "name": "Late"}]}"#)
                .unwrap();
        ui.add_users(page, true);
        assert_eq!(ui.tab().users.len(), 3);
        assert_eq!(ui.tab().selected_user().unwrap().username, "late");
        assert_eq!(ui.tab().next_token, None);
    }

    #[test]
    fn selection_bar_follows_selected_post() {
        let mut ui = loaded_ui();
        // G selects past the end until the list renders; the bar must clamp too.
        ui.tab_mut().list.select_last();
        let screen = screen(&mut ui, 80, 12);
        let rows: Vec<&str> = screen.lines().collect();
        assert!(rows[2].starts_with("  @yunta"));
        assert!(rows[5].starts_with("▌ @yoda"));
        assert!(rows[6].starts_with("▌ RT @gone"));
        assert_eq!(rows[7], "");
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
