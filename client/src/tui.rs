use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    symbols,
    text::{Line, Span},
    widgets::{Axis, Block, Borders, Chart, Dataset, Gauge, List, ListItem, Paragraph, Sparkline},
    Frame, Terminal,
};
use shared::{LogBuffer, MetricsCollector};
use std::io;
use std::sync::Arc;
use std::time::{Duration, Instant};

pub struct ClientTui {
    metrics: Arc<MetricsCollector>,
    log_buffer: LogBuffer,
    should_quit: bool,
    show_logs: bool,
    log_scroll: usize,
    throughput_history: Vec<(f64, f64, f64, f64)>,
    latency_history: Vec<(f64, f64)>,
    error_history: Vec<(f64, f64)>,
    start_time: Instant,
}

impl ClientTui {
    pub fn new(metrics: Arc<MetricsCollector>, log_buffer: LogBuffer) -> Self {
        Self {
            metrics,
            log_buffer,
            should_quit: false,
            show_logs: false,
            log_scroll: 0,
            throughput_history: Vec::new(),
            latency_history: Vec::new(),
            error_history: Vec::new(),
            start_time: Instant::now(),
        }
    }

    pub async fn run(&mut self) -> anyhow::Result<()> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();

        // Completely redirect stderr to null in TUI mode to prevent any leakage
        let _stderr_redirect = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open("/dev/null")
            .ok();

        execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;

        let tick_rate = Duration::from_millis(250);
        let mut last_tick = Instant::now();

        loop {
            terminal.draw(|f| self.render_ui(f))?;

            let timeout = tick_rate
                .checked_sub(last_tick.elapsed())
                .unwrap_or_else(|| Duration::from_secs(0));

            if crossterm::event::poll(timeout)? {
                if let Event::Key(key) = event::read()? {
                    match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => {
                            self.should_quit = true;
                        }
                        KeyCode::Char('l') | KeyCode::Char('L') => {
                            self.show_logs = !self.show_logs;
                            self.log_scroll = 0;
                        }
                        KeyCode::Up => {
                            if self.show_logs && self.log_scroll > 0 {
                                self.log_scroll -= 1;
                            }
                        }
                        KeyCode::Down => {
                            if self.show_logs {
                                let log_count = self.log_buffer.len();
                                if self.log_scroll < log_count.saturating_sub(10) {
                                    self.log_scroll += 1;
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }

            if last_tick.elapsed() >= tick_rate {
                self.update_metrics();
                last_tick = Instant::now();
            }

            if self.should_quit {
                break;
            }
        }

        disable_raw_mode()?;
        execute!(
            terminal.backend_mut(),
            LeaveAlternateScreen,
            DisableMouseCapture
        )?;
        terminal.show_cursor()?;

        Ok(())
    }

    fn update_metrics(&mut self) {
        let elapsed = self.start_time.elapsed().as_secs_f64();

        if let Some(throughput) = self.metrics.get_latest_throughput() {
            self.throughput_history.push((
                elapsed,
                throughput.tcp_upload_bps as f64 / 1_000_000.0,
                throughput.tcp_download_bps as f64 / 1_000_000.0,
                (throughput.udp_upload_bps + throughput.udp_download_bps) as f64 / 1_000_000.0,
            ));
        }

        if let Some(latency) = self.metrics.get_latest_latency() {
            self.latency_history.push((elapsed, latency.tcp_latency_ms));
        }

        if let Some(errors) = self.metrics.get_latest_errors() {
            self.error_history
                .push((elapsed, (errors.tcp_errors + errors.udp_errors) as f64));
        }

        const MAX_HISTORY_SIZE: usize = 120;
        if self.throughput_history.len() > MAX_HISTORY_SIZE {
            self.throughput_history.remove(0);
        }
        if self.latency_history.len() > MAX_HISTORY_SIZE {
            self.latency_history.remove(0);
        }
        if self.error_history.len() > MAX_HISTORY_SIZE {
            self.error_history.remove(0);
        }
    }

    fn render_ui(&self, f: &mut Frame) {
        let main_constraints = if self.show_logs {
            vec![
                Constraint::Length(3),
                Constraint::Length(8),
                Constraint::Min(8),
                Constraint::Length(10),
                Constraint::Length(3),
            ]
        } else {
            vec![
                Constraint::Length(3),
                Constraint::Length(8),
                Constraint::Min(8),
                Constraint::Length(3),
            ]
        };

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .margin(1)
            .constraints(main_constraints.as_slice())
            .split(f.size());

        self.render_header(f, chunks[0]);
        self.render_connection_stats(f, chunks[1]);

        if self.show_logs {
            self.render_charts(f, chunks[2]);
            self.render_logs(f, chunks[3]);
            self.render_footer(f, chunks[4]);
        } else {
            self.render_charts(f, chunks[2]);
            self.render_footer(f, chunks[3]);
        }
    }

    fn render_header(&self, f: &mut Frame, area: Rect) {
        let elapsed = self.start_time.elapsed();
        let active_users = self.metrics.get_active_users();
        let total_users = self.metrics.get_total_users();

        let header = Paragraph::new(vec![Line::from(vec![
            Span::styled(
                "NPT Client Dashboard",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  |  "),
            Span::styled(
                format!(
                    "Runtime: {:02}:{:02}:{:02}",
                    elapsed.as_secs() / 3600,
                    (elapsed.as_secs() % 3600) / 60,
                    elapsed.as_secs() % 60
                ),
                Style::default().fg(Color::Green),
            ),
            Span::raw("  |  "),
            Span::styled(
                format!("Users: {}/{}", active_users, total_users),
                Style::default().fg(Color::Cyan),
            ),
        ])])
        .block(Block::default().borders(Borders::ALL).title("Status"))
        .style(Style::default().fg(Color::White));

        f.render_widget(header, area);
    }

    fn render_connection_stats(&self, f: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints(
                [
                    Constraint::Percentage(20),
                    Constraint::Percentage(20),
                    Constraint::Percentage(20),
                    Constraint::Percentage(20),
                    Constraint::Percentage(20),
                ]
                .as_ref(),
            )
            .split(area);

        let web_browsing_tasks = self.metrics.get_active_web_browsing_tasks();
        let file_download_tasks = self.metrics.get_active_file_download_tasks();
        let file_upload_tasks = self.metrics.get_active_file_upload_tasks();

        let web_browsing_gauge = Gauge::default()
            .block(Block::default().borders(Borders::ALL).title("网页浏览"))
            .gauge_style(Style::default().fg(Color::Blue))
            .percent(std::cmp::min(web_browsing_tasks * 100 / 50, 100) as u16)
            .label(format!("{} 任务", web_browsing_tasks));

        f.render_widget(web_browsing_gauge, chunks[0]);

        let file_download_gauge = Gauge::default()
            .block(Block::default().borders(Borders::ALL).title("文件下载"))
            .gauge_style(Style::default().fg(Color::Green))
            .percent(std::cmp::min(file_download_tasks * 100 / 50, 100) as u16)
            .label(format!("{} 任务", file_download_tasks));

        f.render_widget(file_download_gauge, chunks[1]);

        let file_upload_gauge = Gauge::default()
            .block(Block::default().borders(Borders::ALL).title("文件上传"))
            .gauge_style(Style::default().fg(Color::Red))
            .percent(std::cmp::min(file_upload_tasks * 100 / 50, 100) as u16)
            .label(format!("{} 任务", file_upload_tasks));

        f.render_widget(file_upload_gauge, chunks[2]);

        let latest_throughput = self.metrics.get_latest_throughput();
        let current_mb = if let Some(tp) = latest_throughput {
            (tp.tcp_upload_bps + tp.tcp_download_bps + tp.udp_upload_bps + tp.udp_download_bps)
                as f64
                / 1_000_000.0
        } else {
            0.0
        };

        let throughput_gauge = Gauge::default()
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Current Throughput"),
            )
            .gauge_style(Style::default().fg(Color::Yellow))
            .percent(std::cmp::min(current_mb as u16, 100))
            .label(format!("{:.2} MB", current_mb));

        f.render_widget(throughput_gauge, chunks[3]);

        let latest_errors = self.metrics.get_latest_errors();
        let error_count = if let Some(errors) = latest_errors {
            errors.tcp_errors + errors.udp_errors
        } else {
            0
        };

        let error_gauge = Gauge::default()
            .block(Block::default().borders(Borders::ALL).title("Total Errors"))
            .gauge_style(Style::default().fg(Color::Red))
            .percent(std::cmp::min(error_count as u16, 100))
            .label(format!("{}", error_count));

        f.render_widget(error_gauge, chunks[4]);
    }

    fn render_charts(&self, f: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)].as_ref())
            .split(area);

        let left_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(70), Constraint::Percentage(30)].as_ref())
            .split(chunks[0]);

        let right_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)].as_ref())
            .split(chunks[1]);

        self.render_throughput_chart(f, left_chunks[0]);
        self.render_connection_sparkline(f, left_chunks[1]);
        self.render_latency_chart(f, right_chunks[0]);
        self.render_error_chart(f, right_chunks[1]);
    }

    fn render_throughput_chart(&self, f: &mut Frame, area: Rect) {
        if self.throughput_history.is_empty() {
            let empty_chart = Paragraph::new("No throughput data available")
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title("Throughput (MB)"),
                )
                .style(Style::default().fg(Color::Gray));
            f.render_widget(empty_chart, area);
            return;
        }

        let tcp_upload_data: Vec<(f64, f64)> = self
            .throughput_history
            .iter()
            .map(|(time, upload, _, _)| (*time, *upload))
            .collect();

        let tcp_download_data: Vec<(f64, f64)> = self
            .throughput_history
            .iter()
            .map(|(time, _, download, _)| (*time, *download))
            .collect();

        let udp_data: Vec<(f64, f64)> = self
            .throughput_history
            .iter()
            .map(|(time, _, _, udp)| (*time, *udp))
            .collect();

        let max_throughput = self
            .throughput_history
            .iter()
            .map(|(_, upload, download, udp)| upload + download + udp)
            .fold(0.0f64, f64::max)
            .max(1.0);

        let datasets = vec![
            Dataset::default()
                .name("TCP Upload")
                .marker(symbols::Marker::Dot)
                .style(Style::default().fg(Color::Red))
                .data(&tcp_upload_data),
            Dataset::default()
                .name("TCP Download")
                .marker(symbols::Marker::Dot)
                .style(Style::default().fg(Color::Blue))
                .data(&tcp_download_data),
            Dataset::default()
                .name("UDP")
                .marker(symbols::Marker::Dot)
                .style(Style::default().fg(Color::Green))
                .data(&udp_data),
        ];

        let min_time = self.throughput_history.first().map(|x| x.0).unwrap_or(0.0);
        let max_time = self.throughput_history.last().map(|x| x.0).unwrap_or(1.0);

        let chart = Chart::new(datasets)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Throughput (MB)"),
            )
            .x_axis(
                Axis::default()
                    .title("Time (s)")
                    .style(Style::default().fg(Color::Gray))
                    .bounds([min_time, max_time])
                    .labels(vec![
                        Span::styled(format!("{:.0}", min_time), Style::default()),
                        Span::styled(format!("{:.0}", max_time), Style::default()),
                    ]),
            )
            .y_axis(
                Axis::default()
                    .title("MB")
                    .style(Style::default().fg(Color::Gray))
                    .bounds([0.0, max_throughput])
                    .labels(vec![
                        Span::styled("0", Style::default()),
                        Span::styled(format!("{:.1}", max_throughput), Style::default()),
                    ]),
            );

        f.render_widget(chart, area);
    }

    fn render_latency_chart(&self, f: &mut Frame, area: Rect) {
        if self.latency_history.is_empty() {
            let empty_chart = Paragraph::new("No latency data available")
                .block(Block::default().borders(Borders::ALL).title("Latency (ms)"))
                .style(Style::default().fg(Color::Gray));
            f.render_widget(empty_chart, area);
            return;
        }

        let max_latency = self
            .latency_history
            .iter()
            .map(|(_, latency)| *latency)
            .fold(0.0f64, f64::max)
            .max(1.0);

        let dataset = vec![Dataset::default()
            .name("Latency")
            .marker(symbols::Marker::Braille)
            .style(Style::default().fg(Color::Yellow))
            .data(&self.latency_history)];

        let min_time = self.latency_history.first().map(|x| x.0).unwrap_or(0.0);
        let max_time = self.latency_history.last().map(|x| x.0).unwrap_or(1.0);

        let chart = Chart::new(dataset)
            .block(Block::default().borders(Borders::ALL).title("Latency (ms)"))
            .x_axis(
                Axis::default()
                    .title("Time (s)")
                    .style(Style::default().fg(Color::Gray))
                    .bounds([min_time, max_time]),
            )
            .y_axis(
                Axis::default()
                    .title("ms")
                    .style(Style::default().fg(Color::Gray))
                    .bounds([0.0, max_latency])
                    .labels(vec![
                        Span::styled("0", Style::default()),
                        Span::styled(format!("{:.1}", max_latency), Style::default()),
                    ]),
            );

        f.render_widget(chart, area);
    }

    fn render_error_chart(&self, f: &mut Frame, area: Rect) {
        if self.error_history.is_empty() {
            let empty_chart = Paragraph::new("No error data available")
                .block(Block::default().borders(Borders::ALL).title("Errors"))
                .style(Style::default().fg(Color::Gray));
            f.render_widget(empty_chart, area);
            return;
        }

        let max_errors = self
            .error_history
            .iter()
            .map(|(_, errors)| *errors)
            .fold(0.0f64, f64::max)
            .max(1.0);

        let dataset = vec![Dataset::default()
            .name("Errors")
            .marker(symbols::Marker::Braille)
            .style(Style::default().fg(Color::Red))
            .data(&self.error_history)];

        let min_time = self.error_history.first().map(|x| x.0).unwrap_or(0.0);
        let max_time = self.error_history.last().map(|x| x.0).unwrap_or(1.0);

        let chart = Chart::new(dataset)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Cumulative Errors"),
            )
            .x_axis(
                Axis::default()
                    .title("Time (s)")
                    .style(Style::default().fg(Color::Gray))
                    .bounds([min_time, max_time]),
            )
            .y_axis(
                Axis::default()
                    .title("Count")
                    .style(Style::default().fg(Color::Gray))
                    .bounds([0.0, max_errors]),
            );

        f.render_widget(chart, area);
    }

    fn render_connection_sparkline(&self, f: &mut Frame, area: Rect) {
        let connection_data: Vec<u64> = self
            .throughput_history
            .iter()
            .map(|_| {
                (self.metrics.get_active_tcp_connections()
                    + self.metrics.get_active_udp_connections()) as u64
            })
            .collect();

        if connection_data.is_empty() {
            let empty_sparkline = Paragraph::new("No connection data")
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title("Connection History"),
                )
                .style(Style::default().fg(Color::Gray));
            f.render_widget(empty_sparkline, area);
            return;
        }

        let sparkline = Sparkline::default()
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Connection History"),
            )
            .data(&connection_data)
            .style(Style::default().fg(Color::Cyan));

        f.render_widget(sparkline, area);
    }

    fn render_logs(&self, f: &mut Frame, area: Rect) {
        let log_entries = self.log_buffer.get_entries();
        let total_logs = log_entries.len();

        let visible_logs = 8; // Number of log lines to show
        let start_index = if total_logs > visible_logs {
            let max_scroll = total_logs.saturating_sub(visible_logs);
            let scroll = self.log_scroll.min(max_scroll);
            total_logs
                .saturating_sub(visible_logs)
                .saturating_sub(scroll)
        } else {
            0
        };

        let displayed_logs: Vec<ListItem> = log_entries
            .iter()
            .skip(start_index)
            .take(visible_logs)
            .map(|entry| {
                let time_str = entry.timestamp.format("%H:%M:%S").to_string();
                let level_symbol = entry.level_symbol();
                let (r, g, b) = entry.level_color();

                let line = Line::from(vec![
                    Span::styled(
                        format!("{} ", time_str),
                        Style::default().fg(Color::Rgb(128, 128, 128)),
                    ),
                    Span::styled(
                        format!("{} ", level_symbol),
                        Style::default().fg(Color::Rgb(r, g, b)),
                    ),
                    Span::styled(entry.message.clone(), Style::default().fg(Color::White)),
                ]);
                ListItem::new(line)
            })
            .collect();

        let title = if total_logs > visible_logs {
            format!(
                "Logs ({}/{}) - ↑↓ to scroll",
                start_index + displayed_logs.len().min(visible_logs),
                total_logs
            )
        } else {
            format!("Logs ({})", total_logs)
        };

        let list = List::new(displayed_logs)
            .block(Block::default().borders(Borders::ALL).title(title))
            .style(Style::default().fg(Color::White));

        f.render_widget(list, area);
    }

    fn render_footer(&self, f: &mut Frame, area: Rect) {
        let instructions = if self.show_logs {
            "Press 'q' to quit | 'L' to hide logs | ↑↓ to scroll logs"
        } else {
            "Press 'q' to quit | 'L' to show logs"
        };

        let footer = Paragraph::new(instructions)
            .block(Block::default().borders(Borders::ALL))
            .style(Style::default().fg(Color::White));

        f.render_widget(footer, area);
    }
}
