use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Gauge, Paragraph, Row, Sparkline, Table},
};
use crate::app::{AppState, SortMode};
use crate::data::{avg_cpu, top_processes};

pub fn render(frame: &mut Frame, state: &AppState) {
    let area = frame.area();

    if area.width < 60 || area.height < 20 {
        let msg = Paragraph::new("Terminal too small — resize to at least 60x20")
            .style(Style::default().fg(Color::Red));
        frame.render_widget(msg, area);
        return;
    }

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(5),
            Constraint::Length(6),
            Constraint::Min(10),
            Constraint::Length(1),
        ])
        .split(area);

    render_top_row(frame, state, rows[0]);
    render_mid_row(frame, state, rows[1]);
    render_bottom_row(frame, state, rows[2]);
    render_status_bar(frame, state, rows[3]);
}

fn render_top_row(frame: &mut Frame, state: &AppState, area: Rect) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
        .split(area);

    // CPU panel: sparkline on top, gauge on bottom
    let cpu_panels = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Length(3)])
        .split(cols[0]);

    let cpu_data: Vec<u64> = state.cpu_history.iter().copied().collect();
    let sparkline = Sparkline::default()
        .block(Block::default().title("CPU History").borders(Borders::TOP | Borders::LEFT | Borders::RIGHT))
        .data(&cpu_data)
        .max(100)
        .style(Style::default().fg(Color::Cyan));
    frame.render_widget(sparkline, cpu_panels[0]);

    let cpu_pct = avg_cpu(&state.sys);
    let cpu_ratio = (cpu_pct as f64 / 100.0).clamp(0.0, 1.0);
    let cpu_color = if cpu_pct > 80.0 { Color::Red } else if cpu_pct > 50.0 { Color::Yellow } else { Color::Green };
    let cpu_gauge = Gauge::default()
        .block(Block::default().borders(Borders::LEFT | Borders::BOTTOM | Borders::RIGHT))
        .ratio(cpu_ratio)
        .label(format!("{:.1}%", cpu_pct))
        .gauge_style(Style::default().fg(cpu_color));
    frame.render_widget(cpu_gauge, cpu_panels[1]);

    // RAM gauge
    let total_ram = state.sys.total_memory();
    let used_ram = state.sys.used_memory();
    let ram_ratio = if total_ram > 0 { used_ram as f64 / total_ram as f64 } else { 0.0 };
    let ram_pct = ram_ratio * 100.0;
    let ram_color = if ram_pct > 80.0 { Color::Red } else if ram_pct > 60.0 { Color::Yellow } else { Color::Blue };
    let ram_gauge = Gauge::default()
        .block(Block::default().title("RAM").borders(Borders::ALL))
        .ratio(ram_ratio.clamp(0.0, 1.0))
        .label(format!("{} / {} MB", used_ram / 1_048_576, total_ram / 1_048_576))
        .gauge_style(Style::default().fg(ram_color));
    frame.render_widget(ram_gauge, cols[1]);
}

fn render_mid_row(frame: &mut Frame, state: &AppState, area: Rect) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);

    // Disk gauges stacked
    let valid_disks: Vec<_> = state.disks.iter().filter(|d| d.total_space() > 0).collect();
    let disk_block = Block::default().title("Disks").borders(Borders::ALL);
    let inner = disk_block.inner(cols[0]);
    frame.render_widget(disk_block, cols[0]);

    if valid_disks.is_empty() {
        let msg = Paragraph::new("No disks found");
        frame.render_widget(msg, inner);
    } else {
        let n = valid_disks.len().min(inner.height as usize);
        let constraints: Vec<Constraint> = (0..n).map(|_| Constraint::Length(1)).collect();
        let disk_areas = Layout::default()
            .direction(Direction::Vertical)
            .constraints(constraints)
            .split(inner);

        for (i, disk) in valid_disks.iter().take(n).enumerate() {
            let total = disk.total_space();
            let free = disk.available_space();
            let used_pct = (total - free) as f64 / total as f64 * 100.0;
            let disk_color = if used_pct > 80.0 { Color::Red } else { Color::Yellow };
            let name = disk.mount_point().to_string_lossy();
            let gauge = Gauge::default()
                .ratio((used_pct / 100.0).clamp(0.0, 1.0))
                .label(format!("{} {:.0}% ({} GB free)", name, used_pct, free / 1_073_741_824))
                .gauge_style(Style::default().fg(disk_color));
            frame.render_widget(gauge, disk_areas[i]);
        }
    }

    // Network sparklines
    let net_panels = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(cols[1]);

    let rx_data: Vec<u64> = state.net_rx_history.iter().copied().collect();
    let tx_data: Vec<u64> = state.net_tx_history.iter().copied().collect();
    let rx_max = rx_data.iter().copied().max().unwrap_or(1).max(1);
    let tx_max = tx_data.iter().copied().max().unwrap_or(1).max(1);

    let rx_spark = Sparkline::default()
        .block(Block::default().title("Net ↓ RX").borders(Borders::TOP | Borders::LEFT | Borders::RIGHT))
        .data(&rx_data)
        .max(rx_max)
        .style(Style::default().fg(Color::Blue));
    frame.render_widget(rx_spark, net_panels[0]);

    let tx_spark = Sparkline::default()
        .block(Block::default().title("Net ↑ TX").borders(Borders::ALL))
        .data(&tx_data)
        .max(tx_max)
        .style(Style::default().fg(Color::Magenta));
    frame.render_widget(tx_spark, net_panels[1]);
}

fn render_bottom_row(frame: &mut Frame, state: &AppState, area: Rect) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(65), Constraint::Percentage(35)])
        .split(area);

    // Process table
    let sort_label = match state.sort_mode {
        SortMode::Cpu => "CPU",
        SortMode::Mem => "MEM",
    };
    let cpu_col_header = format!("CPU% [{}]", sort_label);
    let header = Row::new(vec!["PID", "Name", cpu_col_header.as_str(), "MEM MB"])
        .style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD));

    let procs = top_processes(state, 10);
    let rows: Vec<Row> = procs
        .iter()
        .map(|p| {
            Row::new(vec![
                p.pid.to_string(),
                p.name.clone(),
                format!("{:.1}", p.cpu_pct),
                p.mem_mb.to_string(),
            ])
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(7),
            Constraint::Min(12),
            Constraint::Length(12),
            Constraint::Length(8),
        ],
    )
    .header(header)
    .block(Block::default().title("Processes (top 10)").borders(Borders::ALL))
    .column_spacing(1);
    frame.render_widget(table, cols[0]);

    // Temperature panel
    let temp_block = Block::default().title("Temperatures").borders(Borders::ALL);
    let temp_inner = temp_block.inner(cols[1]);
    frame.render_widget(temp_block, cols[1]);

    let temp_rows: Vec<Row> = state
        .components
        .iter()
        .filter_map(|c| {
            c.temperature().map(|t| Row::new(vec![c.label().to_string(), format!("{:.0}°C", t)]))
        })
        .collect();

    if temp_rows.is_empty() {
        let msg = Paragraph::new("No sensors available");
        frame.render_widget(msg, temp_inner);
    } else {
        let temp_table = Table::new(
            temp_rows,
            [Constraint::Min(10), Constraint::Length(8)],
        )
        .block(Block::default())
        .column_spacing(1);
        frame.render_widget(temp_table, temp_inner);
    }
}

fn render_status_bar(frame: &mut Frame, state: &AppState, area: Rect) {
    let sort_label = match state.sort_mode {
        SortMode::Cpu => "CPU",
        SortMode::Mem => "MEM",
    };
    let line = Line::from(vec![
        Span::styled(" [q] ", Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)),
        Span::raw("quit  "),
        Span::styled("[p] ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
        Span::raw(format!("sort: {}  ", sort_label)),
        Span::styled("[r] ", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
        Span::raw(format!("refresh  interval: {}s", state.interval.as_secs())),
    ]);
    let bar = Paragraph::new(line).style(Style::default().bg(Color::DarkGray));
    frame.render_widget(bar, area);
}
