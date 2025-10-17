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
    throughput_history: Vec<(f64, f64, f64, f64, f64, f64)>,
    latency_history: Vec<(f64, f64)>,
    error_history: Vec<(f64, f64)>,
    start_time: Instant,
    cached_datasets: Option<CachedChartData>,
    last_metrics_update: Instant,
}

#[derive(Clone)]
struct CachedChartData {
    tcp_upload_data: Vec<(f64, f64)>,
    tcp_download_data: Vec<(f64, f64)>,
    udp_data: Vec<(f64, f64)>,
    quic_upload_data: Vec<(f64, f64)>,
    quic_download_data: Vec<(f64, f64)>,
    latency_data: Vec<(f64, f64)>,
    error_data: Vec<(f64, f64)>,
    connection_data: Vec<u64>,
    last_update: Instant,
}

impl ClientTui {
    pub fn new(metrics: Arc<MetricsCollector>, log_buffer: LogBuffer) -> Self {
        let now = Instant::now();
        Self {
            metrics,
            log_buffer,
            should_quit: false,
            show_logs: false,
            log_scroll: 0,
            throughput_history: Vec::new(),
            latency_history: Vec::new(),
            error_history: Vec::new(),
            start_time: now,
            cached_datasets: None,
            last_metrics_update: now,
        }
    }

    pub fn run(&mut self) -> anyhow::Result<()> {
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

        let input_check_rate = Duration::from_millis(50); // Check input frequently
        let render_rate = Duration::from_millis(1000); // Render less frequently
        let metrics_update_rate = Duration::from_millis(100); // Update metrics less frequently

        let mut last_render = Instant::now();
        let mut last_metrics_update = Instant::now();
        let mut force_render = true;

        loop {
            // Always check for input events first with minimal timeout
            if crossterm::event::poll(input_check_rate)? {
                if let Event::Key(key) = event::read()? {
                    match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => {
                            self.should_quit = true;
                            break;
                        }
                        KeyCode::Char('l') | KeyCode::Char('L') => {
                            self.show_logs = !self.show_logs;
                            self.log_scroll = 0;
                            force_render = true; // Force immediate render on layout change
                        }
                        KeyCode::Up => {
                            if self.show_logs && self.log_scroll > 0 {
                                self.log_scroll -= 1;
                                force_render = true;
                            }
                        }
                        KeyCode::Down => {
                            if self.show_logs {
                                let log_count = self.log_buffer.len();
                                if self.log_scroll < log_count.saturating_sub(10) {
                                    self.log_scroll += 1;
                                    force_render = true;
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }

            if self.should_quit {
                break;
            }

            // Update metrics less frequently
            if last_metrics_update.elapsed() >= metrics_update_rate {
                self.update_metrics();
                last_metrics_update = Instant::now();
                force_render = true;
            }

            // Render less frequently or when forced
            if force_render || last_render.elapsed() >= render_rate {
                terminal.draw(|f| self.render_ui_immutable(f))?;
                last_render = Instant::now();
                force_render = false;
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
        let mut data_changed = false;

        if let Some(throughput) = self
            .metrics
            .try_get_latest_throughput()
            .or_else(|| self.metrics.get_latest_throughput())
        {
            self.throughput_history.push((
                elapsed,
                throughput.tcp_upload_bps as f64 / 1_000_000.0,
                throughput.tcp_download_bps as f64 / 1_000_000.0,
                (throughput.udp_upload_bps + throughput.udp_download_bps) as f64 / 1_000_000.0,
                throughput.quic_upload_bps as f64 / 1_000_000.0,
                throughput.quic_download_bps as f64 / 1_000_000.0,
            ));
            data_changed = true;
        }

        if let Some(latency) = self
            .metrics
            .try_get_latest_latency()
            .or_else(|| self.metrics.get_latest_latency())
        {
            self.latency_history.push((elapsed, latency.tcp_latency_ms));
            data_changed = true;
        }

        if let Some(errors) = self
            .metrics
            .try_get_latest_errors()
            .or_else(|| self.metrics.get_latest_errors())
        {
            self.error_history.push((
                elapsed,
                (errors.tcp_errors + errors.udp_errors + errors.quic_errors) as f64,
            ));
            data_changed = true;
        }

        const MAX_HISTORY_SIZE: usize = 120;
        if self.throughput_history.len() > MAX_HISTORY_SIZE {
            self.throughput_history.remove(0);
            data_changed = true;
        }
        if self.latency_history.len() > MAX_HISTORY_SIZE {
            self.latency_history.remove(0);
            data_changed = true;
        }
        if self.error_history.len() > MAX_HISTORY_SIZE {
            self.error_history.remove(0);
            data_changed = true;
        }

        // Invalidate and update cache when data changes
        if data_changed {
            self.cached_datasets = None;
            self.last_metrics_update = Instant::now();
            // Update cache immediately after metrics update
            self.get_or_update_cached_datasets();
        }
    }

    fn get_or_update_cached_datasets(&mut self) -> &CachedChartData {
        let should_update = self.cached_datasets.is_none()
            || self.cached_datasets.as_ref().map_or(true, |cache| {
                cache.last_update.elapsed() > Duration::from_millis(2000)
            });

        if should_update {
            // Pre-calculate all chart datasets
            let tcp_upload_data: Vec<(f64, f64)> = self
                .throughput_history
                .iter()
                .map(|(time, upload, _, _, _, _)| (*time, *upload))
                .collect();

            let tcp_download_data: Vec<(f64, f64)> = self
                .throughput_history
                .iter()
                .map(|(time, _, download, _, _, _)| (*time, *download))
                .collect();

            let udp_data: Vec<(f64, f64)> = self
                .throughput_history
                .iter()
                .map(|(time, _, _, udp, _, _)| (*time, *udp))
                .collect();

            let quic_upload_data: Vec<(f64, f64)> = self
                .throughput_history
                .iter()
                .map(|(time, _, _, _, quic_up, _)| (*time, *quic_up))
                .collect();

            let quic_download_data: Vec<(f64, f64)> = self
                .throughput_history
                .iter()
                .map(|(time, _, _, _, _, quic_down)| (*time, *quic_down))
                .collect();

            let latency_data = self.latency_history.clone();
            let error_data = self.error_history.clone();

            let connection_data: Vec<u64> = (0..self.throughput_history.len())
                .map(|_| {
                    (self.metrics.get_active_tcp_connections()
                        + self.metrics.get_active_udp_connections()
                        + self.metrics.get_active_quic_connections()) as u64
                })
                .collect();

            self.cached_datasets = Some(CachedChartData {
                tcp_upload_data,
                tcp_download_data,
                udp_data,
                quic_upload_data,
                quic_download_data,
                latency_data,
                error_data,
                connection_data,
                last_update: Instant::now(),
            });
        }

        self.cached_datasets
            .as_ref()
            .expect("Cache should be initialized at this point")
    }

    fn render_ui_immutable(&self, f: &mut Frame) {
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
            self.render_charts_immutable(f, chunks[2]);
            self.render_logs(f, chunks[3]);
            self.render_footer(f, chunks[4]);
        } else {
            self.render_charts_immutable(f, chunks[2]);
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
                    Constraint::Percentage(16),
                    Constraint::Percentage(16),
                    Constraint::Percentage(17),
                    Constraint::Percentage(17),
                    Constraint::Percentage(17),
                    Constraint::Percentage(17),
                ]
                .as_ref(),
            )
            .split(area);

        let web_browsing_tasks = self.metrics.get_active_web_browsing_tasks();
        let quic_web_browsing_tasks = self.metrics.get_active_quic_web_browsing_tasks();
        let file_download_tasks = self.metrics.get_active_file_download_tasks();
        let file_upload_tasks = self.metrics.get_active_file_upload_tasks();

        let web_browsing_gauge = Gauge::default()
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("TCP网页浏览 •"),
            )
            .gauge_style(Style::default().fg(Color::Red))
            .percent(std::cmp::min(web_browsing_tasks * 100 / 50, 100) as u16)
            .label(format!("{} 任务", web_browsing_tasks));

        f.render_widget(web_browsing_gauge, chunks[0]);

        let quic_web_browsing_gauge = Gauge::default()
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("QUIC网页浏览 ▄"),
            )
            .gauge_style(Style::default().fg(Color::Magenta))
            .percent(std::cmp::min(quic_web_browsing_tasks * 100 / 50, 100) as u16)
            .label(format!("{} 任务", quic_web_browsing_tasks));

        f.render_widget(quic_web_browsing_gauge, chunks[1]);

        let file_download_gauge = Gauge::default()
            .block(Block::default().borders(Borders::ALL).title("文件下载 ▀"))
            .gauge_style(Style::default().fg(Color::Green))
            .percent(std::cmp::min(file_download_tasks * 100 / 50, 100) as u16)
            .label(format!("{} 任务", file_download_tasks));

        f.render_widget(file_download_gauge, chunks[2]);

        let file_upload_gauge = Gauge::default()
            .block(Block::default().borders(Borders::ALL).title("文件上传 ▀"))
            .gauge_style(Style::default().fg(Color::LightRed))
            .percent(std::cmp::min(file_upload_tasks * 100 / 50, 100) as u16)
            .label(format!("{} 任务", file_upload_tasks));

        f.render_widget(file_upload_gauge, chunks[3]);

        let latest_throughput = self
            .metrics
            .try_get_latest_throughput()
            .or_else(|| self.metrics.get_latest_throughput());
        let current_mb = if let Some(tp) = latest_throughput {
            (tp.tcp_upload_bps
                + tp.tcp_download_bps
                + tp.udp_upload_bps
                + tp.udp_download_bps
                + tp.quic_upload_bps
                + tp.quic_download_bps) as f64
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

        f.render_widget(throughput_gauge, chunks[4]);

        let latest_errors = self
            .metrics
            .try_get_latest_errors()
            .or_else(|| self.metrics.get_latest_errors());
        let error_count = if let Some(errors) = latest_errors {
            errors.tcp_errors + errors.udp_errors + errors.quic_errors
        } else {
            0
        };

        let error_gauge = Gauge::default()
            .block(Block::default().borders(Borders::ALL).title("Total Errors"))
            .gauge_style(Style::default().fg(Color::Red))
            .percent(std::cmp::min(error_count as u16, 100))
            .label(format!("{}", error_count));

        f.render_widget(error_gauge, chunks[5]);
    }

    fn render_charts_immutable(&self, f: &mut Frame, area: Rect) {
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

        self.render_throughput_chart_immutable(f, left_chunks[0]);
        self.render_connection_sparkline_immutable(f, left_chunks[1]);
        self.render_latency_chart_immutable(f, right_chunks[0]);
        self.render_error_chart_immutable(f, right_chunks[1]);
    }

    fn render_throughput_chart_immutable(&self, f: &mut Frame, area: Rect) {
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

        let cached_data = match self.cached_datasets.as_ref() {
            Some(data) => data,
            None => {
                // Fallback to empty chart if cache not ready
                let empty_chart = Paragraph::new("Loading chart data...")
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .title("Throughput (MB)"),
                    )
                    .style(Style::default().fg(Color::Gray));
                f.render_widget(empty_chart, area);
                return;
            }
        };
        let tcp_upload_data = &cached_data.tcp_upload_data;
        let tcp_download_data = &cached_data.tcp_download_data;
        let udp_data = &cached_data.udp_data;
        let quic_upload_data = &cached_data.quic_upload_data;
        let quic_download_data = &cached_data.quic_download_data;

        let max_throughput = self
            .throughput_history
            .iter()
            .map(|(_, upload, download, udp, quic_up, quic_down)| {
                upload + download + udp + quic_up + quic_down
            })
            .fold(0.0f64, f64::max)
            .max(1.0);

        let datasets = vec![
            Dataset::default()
                .name("TCP Upload")
                .marker(symbols::Marker::Dot)
                .style(Style::default().fg(Color::Red))
                .data(tcp_upload_data),
            Dataset::default()
                .name("TCP Download")
                .marker(symbols::Marker::Braille)
                .style(Style::default().fg(Color::Blue))
                .data(tcp_download_data),
            Dataset::default()
                .name("UDP")
                .marker(symbols::Marker::Block)
                .style(Style::default().fg(Color::Green))
                .data(udp_data),
            Dataset::default()
                .name("QUIC Upload")
                .marker(symbols::Marker::Bar)
                .style(Style::default().fg(Color::LightRed))
                .data(quic_upload_data),
            Dataset::default()
                .name("QUIC Download")
                .marker(symbols::Marker::HalfBlock)
                .style(Style::default().fg(Color::Magenta))
                .data(quic_download_data),
        ];

        let min_time = self.throughput_history.first().map(|x| x.0).unwrap_or(0.0);
        let max_time = self.throughput_history.last().map(|x| x.0).unwrap_or(1.0);

        let chart = Chart::new(datasets)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Throughput (MB) | •TCP ▀UDP ▄QUIC"),
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

    fn render_latency_chart_immutable(&self, f: &mut Frame, area: Rect) {
        if self.latency_history.is_empty() {
            let empty_chart = Paragraph::new("No latency data available")
                .block(Block::default().borders(Borders::ALL).title("Latency (ms)"))
                .style(Style::default().fg(Color::Gray));
            f.render_widget(empty_chart, area);
            return;
        }

        let cached_data = match self.cached_datasets.as_ref() {
            Some(data) => data,
            None => {
                // Fallback to empty chart if cache not ready
                let empty_chart = Paragraph::new("Loading latency data...")
                    .block(Block::default().borders(Borders::ALL).title("Latency (ms)"))
                    .style(Style::default().fg(Color::Gray));
                f.render_widget(empty_chart, area);
                return;
            }
        };
        let latency_data = &cached_data.latency_data;

        let max_latency = latency_data
            .iter()
            .map(|(_, latency)| *latency)
            .fold(0.0f64, f64::max)
            .max(1.0);

        let dataset = vec![Dataset::default()
            .name("Latency")
            .marker(symbols::Marker::Braille)
            .style(Style::default().fg(Color::Yellow))
            .data(latency_data)];

        let min_time = latency_data.first().map(|x| x.0).unwrap_or(0.0);
        let max_time = latency_data.last().map(|x| x.0).unwrap_or(1.0);

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

    fn render_error_chart_immutable(&self, f: &mut Frame, area: Rect) {
        if self.error_history.is_empty() {
            let empty_chart = Paragraph::new("No error data available")
                .block(Block::default().borders(Borders::ALL).title("Errors"))
                .style(Style::default().fg(Color::Gray));
            f.render_widget(empty_chart, area);
            return;
        }

        let cached_data = match self.cached_datasets.as_ref() {
            Some(data) => data,
            None => {
                // Fallback to empty chart if cache not ready
                let empty_chart = Paragraph::new("Loading error data...")
                    .block(Block::default().borders(Borders::ALL).title("Errors"))
                    .style(Style::default().fg(Color::Gray));
                f.render_widget(empty_chart, area);
                return;
            }
        };
        let error_data = &cached_data.error_data;

        let max_errors = error_data
            .iter()
            .map(|(_, errors)| *errors)
            .fold(0.0f64, f64::max)
            .max(1.0);

        let dataset = vec![Dataset::default()
            .name("Errors")
            .marker(symbols::Marker::Braille)
            .style(Style::default().fg(Color::Red))
            .data(error_data)];

        let min_time = error_data.first().map(|x| x.0).unwrap_or(0.0);
        let max_time = error_data.last().map(|x| x.0).unwrap_or(1.0);

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

    fn render_connection_sparkline_immutable(&self, f: &mut Frame, area: Rect) {
        let cached_data = match self.cached_datasets.as_ref() {
            Some(data) => data,
            None => {
                // Fallback to empty sparkline if cache not ready
                let empty_sparkline = Paragraph::new("Loading connection data...")
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .title("Connection History"),
                    )
                    .style(Style::default().fg(Color::Gray));
                f.render_widget(empty_sparkline, area);
                return;
            }
        };
        let connection_data = &cached_data.connection_data;

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

        let tcp_connections = self.metrics.get_active_tcp_connections();
        let udp_connections = self.metrics.get_active_udp_connections();
        let quic_connections = self.metrics.get_active_quic_connections();

        let sparkline = Sparkline::default()
            .block(Block::default().borders(Borders::ALL).title(format!(
                "Connections | TCP:{} UDP:{} QUIC:{}",
                tcp_connections, udp_connections, quic_connections
            )))
            .data(connection_data)
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
