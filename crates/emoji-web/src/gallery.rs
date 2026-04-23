use crate::terminal_renderer::{TERM_COLS, TERM_ROWS, TerminalGrid};

const GREEN: [u8; 4] = [0, 204, 0, 255];
const BRIGHT: [u8; 4] = [0, 255, 0, 255];
const DIM: [u8; 4] = [0, 102, 0, 255];
const WHITE: [u8; 4] = [255, 255, 255, 255];
const GRAY: [u8; 4] = [160, 160, 160, 255];
const YELLOW: [u8; 4] = [255, 212, 64, 255];
const DIM_GRAY: [u8; 4] = [96, 96, 96, 255];
const BG: [u8; 4] = [0, 0, 0, 255];
const TRANSPARENT: [u8; 4] = [0, 0, 0, 0];
const MENU_DWELL_SECS: f32 = 0.6;

#[derive(Clone, Copy, PartialEq, Eq)]
enum DisplayedMenu {
    Auth,
    Settings,
    Main,
}

pub struct EmojiEntry {
    pub name: String,
}

pub struct Gallery {
    entries: Vec<EmojiEntry>,
    selected: usize,
    search: String,
    preview_index: Option<usize>,
    preview_mix: f32,
    preview_target: f32,
    channel_switch: f32,
    channel_switch_dir: f32,
    channel_switch_loading: bool,
    preview_error: bool,
    preview_reset_nonce: u32,
    auth: HostedAuthState,
    login_request_nonce: u32,
    settings_open: bool,
    sign_out_request_nonce: u32,
    displayed_menu: DisplayedMenu,
    displayed_menu_hold_secs: f32,
}

#[derive(Clone)]
pub struct HostedAuthState {
    pub status: String,
    pub workspace: String,
    pub hint: String,
    pub signed_in: bool,
    pub busy: bool,
    pub auth_configured: bool,
    pub catalog_ready: bool,
    pub auth_prompt: HostedAuthPrompt,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum HostedAuthPrompt {
    None,
    OpenLogin,
}

#[derive(Clone, Copy)]
pub enum KeyAction {
    Up,
    Down,
    PageUp,
    PageDown,
    F2,
    F8,
    Enter,
    Escape,
    Char(char),
    Backspace,
}

impl Gallery {
    pub fn with_entries<I, S>(entries: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            entries: entries
                .into_iter()
                .map(|name| EmojiEntry { name: name.into() })
                .collect(),
            selected: 0,
            search: String::new(),
            preview_index: None,
            preview_mix: 0.0,
            preview_target: 0.0,
            channel_switch: 0.0,
            channel_switch_dir: 0.0,
            channel_switch_loading: false,
            preview_error: false,
            preview_reset_nonce: 0,
            auth: HostedAuthState {
                status: "INITIALIZING".to_owned(),
                workspace: String::new(),
                hint: "Preparing hosted Slack session...".to_owned(),
                signed_in: false,
                busy: true,
                auth_configured: false,
                catalog_ready: false,
                auth_prompt: HostedAuthPrompt::None,
            },
            login_request_nonce: 0,
            settings_open: false,
            sign_out_request_nonce: 0,
            displayed_menu: DisplayedMenu::Auth,
            displayed_menu_hold_secs: MENU_DWELL_SECS,
        }
    }

    pub fn is_previewing(&self) -> bool {
        self.preview_index.is_some()
    }

    pub fn preview_mix(&self) -> f32 {
        self.preview_mix
    }

    pub fn preview_index(&self) -> Option<usize> {
        self.preview_index
    }

    pub fn channel_switch(&self) -> f32 {
        self.channel_switch
    }

    pub fn channel_switch_dir(&self) -> f32 {
        self.channel_switch_dir
    }

    pub fn set_channel_switch_loading(&mut self, loading: bool) {
        self.channel_switch_loading = loading;
        if !loading && self.channel_switch <= 0.0 {
            self.channel_switch_dir = 0.0;
        }
    }

    pub fn set_preview_error(&mut self, error: bool) -> bool {
        let changed = self.preview_error != error;
        self.preview_error = error;
        changed
    }

    pub fn preview_error(&self) -> bool {
        self.preview_error
    }

    pub fn preview_reset_nonce(&self) -> u32 {
        self.preview_reset_nonce
    }

    pub fn login_request_nonce(&self) -> u32 {
        self.login_request_nonce
    }

    pub fn sign_out_request_nonce(&self) -> u32 {
        self.sign_out_request_nonce
    }

    pub fn show_settings_screen(&self) -> bool {
        self.displayed_menu == DisplayedMenu::Settings
    }

    pub fn set_hosted_auth_state(&mut self, auth: HostedAuthState) {
        let should_reset_for_auth_loss = self.auth.catalog_ready && !auth.catalog_ready;
        self.auth = auth;
        if should_reset_for_auth_loss {
            self.preview_index = None;
            self.preview_target = 0.0;
            self.preview_mix = 0.0;
            self.channel_switch = 0.0;
            self.channel_switch_dir = 0.0;
            self.channel_switch_loading = false;
            self.search.clear();
            self.selected = 0;
            self.settings_open = false;
        }
    }

    pub fn show_auth_screen(&self) -> bool {
        self.displayed_menu == DisplayedMenu::Auth
    }

    fn desired_menu(&self) -> DisplayedMenu {
        if self.entries.is_empty()
            && (!self.auth.catalog_ready || self.auth.busy || !self.auth.signed_in)
        {
            DisplayedMenu::Auth
        } else if self.settings_open {
            DisplayedMenu::Settings
        } else {
            DisplayedMenu::Main
        }
    }

    pub fn current_entry_name(&self) -> Option<&str> {
        let filtered = self.filtered_entries();
        if filtered.is_empty() {
            return None;
        }
        let current_filtered_index = if self.preview_target > 0.0 {
            self.preview_index()
                .and_then(|preview_index| {
                    filtered
                        .iter()
                        .position(|(real_index, _)| *real_index == preview_index)
                })
                .unwrap_or(self.selected.min(filtered.len().saturating_sub(1)))
        } else {
            self.selected.min(filtered.len().saturating_sub(1))
        };
        filtered
            .get(current_filtered_index)
            .map(|(_, entry)| entry.name.as_str())
    }

    pub fn set_entries<I, S>(&mut self, entries: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let current_name = self.current_entry_name().map(str::to_owned);
        self.entries = entries
            .into_iter()
            .map(|name| EmojiEntry { name: name.into() })
            .collect();

        let filtered = self.filtered_entries();
        if filtered.is_empty() {
            self.selected = 0;
            self.preview_index = None;
            self.preview_target = 0.0;
            self.preview_mix = 0.0;
            self.channel_switch = 0.0;
            self.channel_switch_dir = 0.0;
            self.channel_switch_loading = false;
            return;
        }

        let next_index = current_name
            .as_deref()
            .and_then(|name| {
                filtered
                    .iter()
                    .position(|(_, entry)| entry.name.as_str() == name)
            })
            .unwrap_or_else(|| self.selected.min(filtered.len().saturating_sub(1)));
        let next_preview_index = if self.preview_index.is_some() {
            filtered.get(next_index).map(|(real_index, _)| *real_index)
        } else {
            None
        };
        self.selected = next_index;
        self.preview_index = next_preview_index;
    }

    pub fn tick(&mut self, dt_secs: f32) {
        let speed = 6.5;
        let delta = (dt_secs * speed).clamp(0.0, 1.0);
        if self.preview_mix < self.preview_target {
            self.preview_mix = (self.preview_mix + delta).min(self.preview_target);
        } else if self.preview_mix > self.preview_target {
            self.preview_mix = (self.preview_mix - delta).max(self.preview_target);
        }

        if self.preview_target <= 0.0 && self.preview_mix <= 0.0 {
            self.preview_index = None;
            self.preview_error = false;
        }

        let switch_decay = (dt_secs * 8.5).clamp(0.0, 1.0);
        if self.channel_switch > 0.0 && !self.channel_switch_loading {
            self.channel_switch = (self.channel_switch - switch_decay).max(0.0);
            if self.channel_switch <= 0.0 {
                self.channel_switch_dir = 0.0;
            }
        }

        self.displayed_menu_hold_secs = (self.displayed_menu_hold_secs - dt_secs).max(0.0);
        let desired_menu = self.desired_menu();
        if desired_menu != self.displayed_menu && self.displayed_menu_hold_secs <= 0.0 {
            self.displayed_menu = desired_menu;
            self.displayed_menu_hold_secs = MENU_DWELL_SECS;
        }
    }

    pub fn handle_key(&mut self, action: KeyAction) {
        if self.show_auth_screen() {
            if matches!(action, KeyAction::Enter)
                && matches!(self.auth.auth_prompt, HostedAuthPrompt::OpenLogin)
            {
                self.login_request_nonce = self.login_request_nonce.wrapping_add(1);
            }
            return;
        }

        if matches!(action, KeyAction::F2) && !self.is_previewing() {
            if self.auth.signed_in {
                self.settings_open = !self.settings_open;
            } else if !self.auth.busy
                && matches!(self.auth.auth_prompt, HostedAuthPrompt::OpenLogin)
            {
                self.login_request_nonce = self.login_request_nonce.wrapping_add(1);
            }
            return;
        }

        if self.settings_open {
            match action {
                KeyAction::Enter => {
                    self.sign_out_request_nonce = self.sign_out_request_nonce.wrapping_add(1);
                    self.settings_open = false;
                }
                KeyAction::Escape => {
                    self.settings_open = false;
                }
                _ => {}
            }
            return;
        }

        if self.is_previewing() {
            match action {
                KeyAction::Up => self.move_preview_selection(-1),
                KeyAction::Down => self.move_preview_selection(1),
                KeyAction::Escape | KeyAction::Backspace => {
                    self.preview_target = 0.0;
                }
                _ => {}
            }
            return;
        }

        match action {
            KeyAction::Up => self.move_selection(-1),
            KeyAction::Down => self.move_selection(1),
            KeyAction::PageUp => self.page_selection(-1),
            KeyAction::PageDown => self.page_selection(1),
            KeyAction::F2 | KeyAction::F8 => {}
            KeyAction::Enter => {
                let filtered = self.filtered_entries();
                if let Some(&(real_index, _)) = filtered.get(self.selected) {
                    self.preview_index = Some(real_index);
                    self.preview_target = 1.0;
                    self.preview_reset_nonce = self.preview_reset_nonce.wrapping_add(1);
                }
            }
            KeyAction::Char(c) => {
                self.search.push(c);
                self.selected = 0;
            }
            KeyAction::Backspace => {
                self.search.pop();
                self.selected = 0;
            }
            KeyAction::Escape => {
                if !self.search.is_empty() {
                    self.search.clear();
                    self.selected = 0;
                }
            }
        }
    }

    pub fn enter_preview_immediate(&mut self) {
        let filtered = self.filtered_entries();
        if let Some(&(real_index, _)) = filtered.get(self.selected) {
            self.preview_index = Some(real_index);
            self.preview_target = 1.0;
            self.preview_mix = 1.0;
            self.preview_reset_nonce = self.preview_reset_nonce.wrapping_add(1);
        }
    }

    pub fn new() -> Self {
        Self::with_entries(
            [
                "thumbsup",
                "heart",
                "fire",
                "rocket",
                "tada",
                "eyes",
                "wave",
                "100",
                "sparkles",
                "pray",
                "muscle",
                "sunglasses",
                "thinking_face",
                "laughing",
                "sob",
                "clap",
                "raised_hands",
                "ok_hand",
                "point_up",
                "star",
                "zap",
                "rainbow",
                "pizza",
                "coffee",
                "beer",
                "skull",
                "ghost",
                "robot_face",
                "alien",
                "unicorn",
                "penguin",
                "cat",
                "dog",
                "parrot",
                "crab",
            ]
            .into_iter()
            .map(str::to_owned),
        )
    }

    fn move_selection(&mut self, delta: isize) {
        let filtered = self.filtered_entries();
        if filtered.is_empty() {
            self.selected = 0;
            return;
        }
        let current = self.selected as isize;
        let len = filtered.len() as isize;
        let next = ((current + delta) % len + len) % len;
        self.selected = next as usize;
    }

    fn page_selection(&mut self, delta_pages: isize) {
        let filtered = self.filtered_entries();
        if filtered.is_empty() {
            self.selected = 0;
            return;
        }
        let page = TERM_ROWS.saturating_sub(4).max(1) as isize;
        let max_index = filtered.len().saturating_sub(1) as isize;
        let next = (self.selected as isize + delta_pages * page).clamp(0, max_index);
        self.selected = next as usize;
    }

    fn move_preview_selection(&mut self, delta: isize) {
        let filtered = self.filtered_entries();
        if filtered.is_empty() {
            return;
        }
        let current = self
            .preview_index
            .and_then(|preview_index| {
                filtered
                    .iter()
                    .position(|(real_index, _)| *real_index == preview_index)
            })
            .unwrap_or(self.selected.min(filtered.len().saturating_sub(1)));
        let max_index = filtered.len().saturating_sub(1) as isize;
        let next = (current as isize + delta).clamp(0, max_index) as usize;
        if next == current {
            return;
        }
        let next_real_index = filtered[next].0;
        self.selected = next;
        self.preview_index = Some(next_real_index);
        self.preview_error = false;
        self.channel_switch = 1.0;
        self.channel_switch_dir = if delta < 0 { -1.0 } else { 1.0 };
        self.channel_switch_loading = true;
        self.preview_reset_nonce = self.preview_reset_nonce.wrapping_add(1);
    }

    pub fn billboard_cell_rect(&self, area_width: u16, area_height: u16) -> Option<CellRect> {
        if !self.is_previewing() || area_width < 2 || area_height < 2 {
            return None;
        }
        Some(CellRect {
            x: 0,
            y: 0,
            width: area_width,
            height: area_height,
        })
    }

    fn filtered_entries(&self) -> Vec<(usize, &EmojiEntry)> {
        let search = self.search.to_ascii_lowercase();
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| {
                search.is_empty() || entry.name.to_ascii_lowercase().contains(&search)
            })
            .collect()
    }
}

#[derive(Clone, Copy)]
pub struct CellRect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

pub fn render_to_grid(grid: &mut TerminalGrid, gallery: &Gallery, time_secs: f64) {
    if gallery.show_auth_screen() {
        grid.clear(BG);
        draw_auth_screen(grid, gallery, time_secs);
    } else if gallery.show_settings_screen() {
        grid.clear(BG);
        draw_settings_screen(grid, gallery, time_secs);
    } else if show_preview_overlay(gallery) {
        grid.clear(TRANSPARENT);
        draw_preview_overlay(grid, gallery);
    } else {
        grid.clear(BG);
        draw_gallery(grid, gallery, time_secs);
    }
}

pub fn show_preview_overlay(gallery: &Gallery) -> bool {
    gallery.is_previewing() && gallery.preview_mix() >= 0.5
}

pub fn cursor_blink_on(time_secs: f64) -> bool {
    ((time_secs * 2.0) as u64) % 2 == 0
}

fn ascii_rule(width: u16) -> String {
    "-".repeat(width as usize)
}

fn put_segments(grid: &mut TerminalGrid, mut x: u16, y: u16, segments: &[(&str, [u8; 4])]) {
    for (text, color) in segments {
        grid.put_text(x, y, text, *color, BG);
        x = x.saturating_add(text.chars().count() as u16);
        if x >= TERM_COLS {
            break;
        }
    }
}

fn put_centered_segments(grid: &mut TerminalGrid, y: u16, segments: &[(&str, [u8; 4])]) {
    let width: usize = segments.iter().map(|(text, _)| text.chars().count()).sum();
    let start_x = ((TERM_COLS as usize).saturating_sub(width)) / 2;
    put_segments(grid, start_x as u16, y, segments);
}

fn put_segments_bg(
    grid: &mut TerminalGrid,
    mut x: u16,
    y: u16,
    bg: [u8; 4],
    segments: &[(&str, [u8; 4])],
) {
    for (text, color) in segments {
        grid.put_text(x, y, text, *color, bg);
        x = x.saturating_add(text.chars().count() as u16);
        if x >= TERM_COLS {
            break;
        }
    }
}

fn draw_gallery(grid: &mut TerminalGrid, gallery: &Gallery, time_secs: f64) {
    draw_header(grid, gallery, time_secs);
    draw_emoji_list(grid, gallery);
    draw_footer(grid, gallery);
}

fn wrap_lines(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();
    for paragraph in text.split('\n') {
        let words: Vec<&str> = paragraph.split_whitespace().collect();
        if words.is_empty() {
            lines.push(String::new());
            continue;
        }
        let mut current = String::new();
        for word in words {
            let next_len = if current.is_empty() {
                word.len()
            } else {
                current.len() + 1 + word.len()
            };
            if next_len > width && !current.is_empty() {
                lines.push(current);
                current = word.to_owned();
            } else {
                if !current.is_empty() {
                    current.push(' ');
                }
                current.push_str(word);
            }
        }
        if !current.is_empty() {
            lines.push(current);
        }
    }
    lines
}

fn draw_auth_screen(grid: &mut TerminalGrid, gallery: &Gallery, time_secs: f64) {
    let cursor = if cursor_blink_on(time_secs) { "_" } else { " " };
    grid.put_centered(4, "ULTRAMOJI VIEWER 4D", BRIGHT, BG);
    grid.put_centered(6, &format!("HOSTED TERMINAL MODE{cursor}"), GREEN, BG);

    if !gallery.auth.workspace.is_empty() {
        grid.put_centered(9, &gallery.auth.workspace, YELLOW, BG);
    }

    grid.put_centered(
        11,
        &gallery.auth.status,
        if gallery.auth.busy { YELLOW } else { WHITE },
        BG,
    );

    let hint_lines = wrap_lines(&gallery.auth.hint, (TERM_COLS as usize).saturating_sub(8));
    for (idx, line) in hint_lines.into_iter().take(5).enumerate() {
        grid.put_centered(13 + idx as u16, &line, DIM_GRAY, BG);
    }

    let action_line = if !gallery.auth.signed_in
        && !gallery.auth.busy
        && matches!(gallery.auth.auth_prompt, HostedAuthPrompt::OpenLogin)
    {
        "ENTER SLACK LOGIN  D DEFAULT EMOJIS"
    } else if !gallery.auth.signed_in && !gallery.auth.busy {
        "PRESS D FOR DEFAULT EMOJIS"
    } else if gallery.auth.busy {
        "LOADING EMOJI CATALOG"
    } else if gallery.auth.signed_in {
        &gallery.auth.status
    } else if gallery.auth.auth_configured {
        "SLACK LOGIN IS UNAVAILABLE"
    } else {
        "SLACK LOGIN IS NOT CONFIGURED"
    };
    grid.put_centered(TERM_ROWS - 4, action_line, BRIGHT, BG);

    let footer = if !gallery.auth.signed_in
        && !gallery.auth.busy
        && matches!(gallery.auth.auth_prompt, HostedAuthPrompt::OpenLogin)
    {
        &[
            ("ENTER", YELLOW),
            (" SLACK LOGIN  ", DIM),
            ("D", YELLOW),
            (" DEFAULT EMOJIS", DIM),
        ][..]
    } else if !gallery.auth.signed_in && !gallery.auth.busy {
        &[("D", YELLOW), (" DEFAULT EMOJIS", DIM)][..]
    } else {
        &[
            ("STATUS", YELLOW),
            (" ", DIM),
            (&gallery.auth.status, DIM_GRAY),
        ][..]
    };
    put_segments(grid, 0, TERM_ROWS - 1, footer);
}

fn draw_settings_screen(grid: &mut TerminalGrid, gallery: &Gallery, time_secs: f64) {
    let cursor = if cursor_blink_on(time_secs) { "_" } else { " " };
    grid.put_centered(5, "SETTINGS", BRIGHT, BG);
    grid.put_centered(7, &format!("TERMINAL MENU{cursor}"), GREEN, BG);

    if !gallery.auth.workspace.is_empty() {
        grid.put_centered(10, &gallery.auth.workspace, YELLOW, BG);
    }

    grid.put_centered(14, "> SIGN OUT OF SLACK", WHITE, BG);
    grid.put_centered(17, "PRESS ENTER TO CONFIRM", BRIGHT, BG);
    grid.put_centered(19, "PRESS ESC TO GO BACK", DIM_GRAY, BG);

    put_segments(
        grid,
        0,
        TERM_ROWS - 1,
        &[
            ("ENTER", BRIGHT),
            (" SIGN OUT  ", DIM),
            ("ESC", BRIGHT),
            (" BACK", DIM),
        ],
    );
}

fn draw_header(grid: &mut TerminalGrid, gallery: &Gallery, time_secs: f64) {
    let cursor = if cursor_blink_on(time_secs) { "_" } else { " " };
    put_segments(
        grid,
        0,
        0,
        &[(" ULTRAMOJI VIEWER 4D", BRIGHT), (cursor, GREEN)],
    );
    let header_hint = if gallery.auth.signed_in {
        Some("F2 SETTINGS")
    } else if !gallery.auth.busy && matches!(gallery.auth.auth_prompt, HostedAuthPrompt::OpenLogin)
    {
        Some("F2 SLACK LOGIN")
    } else {
        None
    };
    if let Some(hint) = header_hint {
        let x = TERM_COLS.saturating_sub(hint.len() as u16);
        grid.put_text(x, 0, hint, DIM_GRAY, BG);
    }
}

fn draw_emoji_list(grid: &mut TerminalGrid, gallery: &Gallery) {
    let filtered = gallery.filtered_entries();
    let count = filtered.len();
    let title = format!(" EMOJI {:>3} ", count);
    let mut rule = ascii_rule(TERM_COLS);
    let title_len = title.len().min(rule.len());
    rule.replace_range(0..title_len, &title[..title_len]);
    grid.put_text(0, 1, &rule, DIM, BG);

    let list_top = 2u16;
    let list_height = TERM_ROWS.saturating_sub(4) as usize;
    if count == 0 {
        grid.put_centered(list_top + 1, "NO EMOJI LOADED", DIM_GRAY, BG);
        grid.put_centered(list_top + 3, "EMOJI CATALOG UNAVAILABLE", DIM, BG);
        let bottom_rule = ascii_rule(TERM_COLS);
        grid.put_text(0, TERM_ROWS - 2, &bottom_rule, DIM, BG);
        return;
    }
    let max_scroll = count.saturating_sub(list_height);
    let scroll = gallery
        .selected
        .saturating_sub(list_height / 2)
        .min(max_scroll);

    for row in 0..list_height {
        let idx = scroll + row;
        if idx >= count {
            break;
        }
        let y = list_top + row as u16;
        let (_, entry) = filtered[idx];
        let selected = idx == gallery.selected;
        draw_entry(grid, y, &entry.name, selected);
    }

    let bottom_rule = ascii_rule(TERM_COLS);
    grid.put_text(0, TERM_ROWS - 2, &bottom_rule, DIM, BG);
}

fn draw_entry(grid: &mut TerminalGrid, y: u16, name: &str, selected: bool) {
    let prefix = if selected { ">" } else { " " };
    let prefix_color = if selected { BRIGHT } else { DIM };
    let name_color = if selected { BRIGHT } else { GREEN };
    put_segments(
        grid,
        0,
        y,
        &[
            (prefix, prefix_color),
            (" :", DIM),
            (name, name_color),
            (":", DIM),
        ],
    );
}

fn draw_footer(grid: &mut TerminalGrid, gallery: &Gallery) {
    if !gallery.search.is_empty() {
        put_segments(
            grid,
            0,
            TERM_ROWS - 1,
            &[(" >", BRIGHT), (&gallery.search, GREEN), ("_", BRIGHT)],
        );
    } else {
        put_segments(
            grid,
            0,
            TERM_ROWS - 1,
            &[
                (" UP/DN", BRIGHT),
                (" MOVE  ", DIM),
                ("PGUP/DN", BRIGHT),
                (" PAGE  ", DIM),
                ("ENTER", BRIGHT),
                (" VIEW", DIM),
            ],
        );
    }
}

fn draw_preview_overlay(grid: &mut TerminalGrid, gallery: &Gallery) {
    let filtered = gallery.filtered_entries();
    if filtered.is_empty() {
        return;
    }
    let current_filtered_index = gallery
        .preview_index()
        .and_then(|preview_index| {
            filtered
                .iter()
                .position(|(real_index, _)| *real_index == preview_index)
        })
        .unwrap_or(gallery.selected.min(filtered.len().saturating_sub(1)));
    let current_name = filtered
        .get(current_filtered_index)
        .map(|(_, entry)| entry.name.as_str())
        .unwrap_or("?");
    let prev_name = current_filtered_index
        .checked_sub(1)
        .and_then(|index| filtered.get(index))
        .map(|(_, entry)| entry.name.as_str());
    let next_name = filtered
        .get(current_filtered_index + 1)
        .map(|(_, entry)| entry.name.as_str());

    let preview_error = gallery.preview_error();
    let label_bg = if preview_error { BG } else { TRANSPARENT };
    let up_line = format!(
        "{}UP  :{}{}",
        if preview_error { " " } else { "" },
        prev_name.unwrap_or("----"),
        if preview_error { ": " } else { ":" },
    );
    let dn_line = format!(
        "{}DN  :{}{}",
        if preview_error { " " } else { "" },
        next_name.unwrap_or("----"),
        if preview_error { ": " } else { ":" },
    );
    let up_x = ((TERM_COLS as usize).saturating_sub(up_line.len())) / 2;
    let dn_x = ((TERM_COLS as usize).saturating_sub(dn_line.len())) / 2;
    put_segments_bg(
        grid,
        up_x as u16,
        1,
        label_bg,
        &[
            (if preview_error { " " } else { "" }, DIM_GRAY),
            (
                "UP",
                if prev_name.is_some() {
                    YELLOW
                } else {
                    DIM_GRAY
                },
            ),
            ("  :", DIM_GRAY),
            (prev_name.unwrap_or("----"), DIM_GRAY),
            (if preview_error { ": " } else { ":" }, DIM_GRAY),
        ],
    );
    let current_label = if preview_error {
        format!(" :{current_name}: ")
    } else {
        format!(":{current_name}:")
    };
    grid.put_centered(3, &current_label, WHITE, label_bg);
    put_segments_bg(
        grid,
        dn_x as u16,
        5,
        label_bg,
        &[
            (if preview_error { " " } else { "" }, DIM_GRAY),
            (
                "DN",
                if next_name.is_some() {
                    YELLOW
                } else {
                    DIM_GRAY
                },
            ),
            ("  :", DIM_GRAY),
            (next_name.unwrap_or("----"), DIM_GRAY),
            (if preview_error { ": " } else { ":" }, DIM_GRAY),
        ],
    );

    if preview_error {
        put_centered_segments(grid, TERM_ROWS / 2, &[("LOAD ", GRAY), ("ERROR", WHITE)]);
        draw_preview_back_hint(grid);
        return;
    }

    draw_preview_back_hint(grid);
}

fn draw_preview_back_hint(grid: &mut TerminalGrid) {
    put_centered_segments(
        grid,
        TERM_ROWS - 2,
        &[("PRESS ", GRAY), ("ESC", WHITE), (" TO GO BACK", GRAY)],
    );
}
