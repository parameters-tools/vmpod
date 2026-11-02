use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{
        Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap,
    },
    Frame, Terminal,
};
use std::io;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::orchestrator::{InstanceSummary, StatusResult, UpParams, VmOrchestrator};

/// Which pane is currently focused or active
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivePane {
    Instances,
    Logs,
}

/// Messages sent from async background workers to update the dashboard
#[derive(Debug, Clone)]
pub enum ActionMessage {
    Status(String, Color),
    Refresh,
}

/// Modal dialog state for user confirmations or forms
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Dialog {
    ConfirmDelete {
        name: String,
    },
    NewVm {
        name: String,
        ram: String,
        smp: String,
        active_field: usize,
    },
}

/// App state representing the interactive dashboard
pub struct App {
    pub orchestrator: Arc<VmOrchestrator>,
    pub instances: Vec<InstanceSummary>,
    pub selected_index: usize,
    pub selected_status: Option<StatusResult>,
    pub logs: Vec<String>,
    pub log_scroll: usize,
    pub status_message: Option<(String, Instant, Color)>,
    pub dialog: Option<Dialog>,
    pub should_quit: bool,
    pub active_pane: ActivePane,
    pub last_refresh: Instant,
    pub list_state: ListState,
}

impl App {
    pub fn new(orchestrator: Arc<VmOrchestrator>) -> Self {
        let mut list_state = ListState::default();
        list_state.select(Some(0));

        Self {
            orchestrator,
            instances: Vec::new(),
            selected_index: 0,
            selected_status: None,
            logs: Vec::new(),
            log_scroll: 0,
            status_message: Some((
                "Dashboard loaded. Press [?] or browse hotkeys above.".to_string(),
                Instant::now(),
                Color::Green,
            )),
            dialog: None,
            should_quit: false,
            active_pane: ActivePane::Instances,
            last_refresh: Instant::now() - Duration::from_secs(10),
            list_state,
        }
    }

    /// Refresh list of instances and update details for current selection
    pub async fn refresh(&mut self) -> Result<()> {
        let instances = self.orchestrator.list_instances().await.unwrap_or_default();
        self.instances = instances;

        if self.instances.is_empty() {
            self.selected_index = 0;
            self.list_state.select(None);
            self.selected_status = None;
            self.logs = vec!["No active instances found. Press [N] to create a new VM.".to_string()];
        } else {
            if self.selected_index >= self.instances.len() {
                self.selected_index = self.instances.len() - 1;
            }
            self.list_state.select(Some(self.selected_index));

            let selected_name = self.instances[self.selected_index].name.clone();
            let is_running = self.instances[self.selected_index].running;

            if let Ok(st) = self.orchestrator.status(&selected_name).await {
                self.selected_status = Some(st);
            } else {
                self.selected_status = None;
            }

            if is_running {
                if let Ok(raw_logs) = self.orchestrator.get_container_logs(&selected_name, 50).await {
                    let lines: Vec<String> = raw_logs
                        .lines()
                        .map(|l| l.to_string())
                        .collect();
                    self.logs = if lines.is_empty() {
                        vec!["[Runner active - waiting for guest kernel output...]".to_string()]
                    } else {
                        lines
                    };
                }
            } else {
                self.logs = vec![
                    format!("VM '{}' is currently stopped.", selected_name),
                    "Press [Space] to start this virtual machine.".to_string(),
                ];
            }
        }

        self.last_refresh = Instant::now();
        Ok(())
    }

    pub fn set_status(&mut self, msg: impl Into<String>, color: Color) {
        self.status_message = Some((msg.into(), Instant::now(), color));
    }

    pub fn move_up(&mut self) {
        if self.instances.is_empty() {
            return;
        }
        if self.selected_index > 0 {
            self.selected_index -= 1;
        } else {
            self.selected_index = self.instances.len() - 1;
        }
        self.list_state.select(Some(self.selected_index));
    }

    pub fn move_down(&mut self) {
        if self.instances.is_empty() {
            return;
        }
        if self.selected_index + 1 < self.instances.len() {
            self.selected_index += 1;
        } else {
            self.selected_index = 0;
        }
        self.list_state.select(Some(self.selected_index));
    }

    pub fn selected_instance(&self) -> Option<&InstanceSummary> {
        self.instances.get(self.selected_index)
    }

    /// Launch SSH into the currently selected instance
    pub fn launch_ssh(&mut self) -> Result<()> {
        let Some(inst) = self.selected_instance() else {
            self.set_status("No VM selected.", Color::Yellow);
            return Ok(());
        };

        if !inst.running {
            self.set_status(
                format!("VM '{}' is stopped. Press [Space] to start it first.", inst.name),
                Color::Yellow,
            );
            return Ok(());
        }

        let Some(port) = inst.port else {
            self.set_status("Instance has no SSH port mapping.", Color::Red);
            return Ok(());
        };

        let key_path = format!("{}/id_ed25519", inst.storage_path);

        // Temporarily leave alternate screen & disable raw mode
        disable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, LeaveAlternateScreen)?;

        println!("\x1b[1;36m[vmpod]\x1b[0m Connecting to VM '{}' on localhost:{}...", inst.name, port);
        println!("\x1b[90m(Type 'exit' to return to the vmpod dashboard)\x1b[0m\n");

        let mut cmd = std::process::Command::new("ssh");
        if Path::new(&key_path).exists() {
            cmd.arg("-i").arg(&key_path);
        }
        cmd.arg("-o").arg("StrictHostKeyChecking=no")
            .arg("-o").arg("UserKnownHostsFile=/dev/null")
            .arg("-o").arg("LogLevel=ERROR")
            .arg("-p").arg(port.to_string())
            .arg("root@localhost");

        let _ = cmd.status();

        execute!(stdout, EnterAlternateScreen)?;
        enable_raw_mode()?;

        self.set_status(format!("Disconnected from '{}'.", inst.name), Color::Green);
        Ok(())
    }

    /// Toggle start/stop for the selected instance in background task
    pub fn toggle_start_stop(&mut self, action_tx: &tokio::sync::mpsc::UnboundedSender<ActionMessage>) {
        let (name, running, storage_path) = {
            let Some(inst) = self.selected_instance() else {
                return;
            };
            (inst.name.clone(), inst.running, inst.storage_path.clone())
        };

        let orchestrator = self.orchestrator.clone();
        let tx = action_tx.clone();

        if running {
            self.set_status(format!("Stopping VM '{}' (flushing disk)...", name), Color::Yellow);
            tokio::spawn(async move {
                match orchestrator.down(&name, false).await {
                    Ok(_) => {
                        let _ = tx.send(ActionMessage::Status(
                            format!("Successfully stopped VM '{}'.", name),
                            Color::Green,
                        ));
                        let _ = tx.send(ActionMessage::Refresh);
                    }
                    Err(e) => {
                        let _ = tx.send(ActionMessage::Status(
                            format!("Error stopping '{}': {:#}", name, e),
                            Color::Red,
                        ));
                        let _ = tx.send(ActionMessage::Refresh);
                    }
                }
            });
        } else {
            self.set_status(format!("Starting VM '{}'...", name), Color::Yellow);
            tokio::spawn(async move {
                let params = UpParams {
                    name: name.clone(),
                    port: None,
                    ram: "2G".to_string(),
                    smp: 2,
                    storage: Some(Path::new(&storage_path).to_path_buf()),
                    image: "vmpod/qemu-runner:latest".to_string(),
                    ephemeral: false,
                };
                match orchestrator.up(params).await {
                    Ok(res) => {
                        let _ = tx.send(ActionMessage::Status(
                            format!("VM '{}' started on port {}.", name, res.port),
                            Color::Green,
                        ));
                        let _ = tx.send(ActionMessage::Refresh);
                    }
                    Err(e) => {
                        let _ = tx.send(ActionMessage::Status(
                            format!("Error starting '{}': {:#}", name, e),
                            Color::Red,
                        ));
                        let _ = tx.send(ActionMessage::Refresh);
                    }
                }
            });
        }
    }

    /// Delete selected instance after user confirmation in background task
    pub fn execute_delete(&mut self, name: String, action_tx: &tokio::sync::mpsc::UnboundedSender<ActionMessage>) {
        self.set_status(format!("Deleting VM '{}'...", name), Color::Yellow);
        let orchestrator = self.orchestrator.clone();
        let tx = action_tx.clone();

        tokio::spawn(async move {
            let _ = orchestrator.down(&name, true).await;
            let path = Path::new("./vmpod-data").join(&name);
            if path.exists() {
                let _ = std::fs::remove_dir_all(&path);
            }
            let _ = tx.send(ActionMessage::Status(
                format!("VM '{}' and storage removed.", name),
                Color::Green,
            ));
            let _ = tx.send(ActionMessage::Refresh);
        });
    }

    /// Create new VM from dialog input in background task
    pub fn execute_create(
        &mut self,
        name: String,
        ram: String,
        smp_str: String,
        action_tx: &tokio::sync::mpsc::UnboundedSender<ActionMessage>,
    ) {
        let smp = smp_str.trim().parse::<u32>().unwrap_or(2);
        self.set_status(format!("Creating and starting VM '{}'...", name), Color::Yellow);
        let orchestrator = self.orchestrator.clone();
        let tx = action_tx.clone();

        tokio::spawn(async move {
            let params = UpParams {
                name: name.clone(),
                port: None,
                ram: if ram.trim().is_empty() { "2G".to_string() } else { ram },
                smp: if smp == 0 { 2 } else { smp },
                storage: None,
                image: "vmpod/qemu-runner:latest".to_string(),
                ephemeral: false,
            };

            match orchestrator.up(params).await {
                Ok(res) => {
                    let _ = tx.send(ActionMessage::Status(
                        format!("VM '{}' successfully launched on port {}.", res.name, res.port),
                        Color::Green,
                    ));
                    let _ = tx.send(ActionMessage::Refresh);
                }
                Err(e) => {
                    let _ = tx.send(ActionMessage::Status(
                        format!("Failed to create VM: {:#}", e),
                        Color::Red,
                    ));
                }
            }
        });
    }

    /// Main event loop handler for keyboard input
    pub async fn handle_key(
        &mut self,
        key: KeyEvent,
        action_tx: &tokio::sync::mpsc::UnboundedSender<ActionMessage>,
    ) -> Result<()> {
        if key.kind != KeyEventKind::Press {
            return Ok(());
        }

        // Handle modal dialog input first if active
        if let Some(dialog) = self.dialog.clone() {
            match dialog {
                Dialog::ConfirmDelete { name } => match key.code {
                    KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                        self.dialog = None;
                        self.execute_delete(name, action_tx);
                    }
                    KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                        self.dialog = None;
                        self.set_status("Deletion cancelled.", Color::DarkGray);
                    }
                    _ => {}
                },
                Dialog::NewVm {
                    mut name,
                    mut ram,
                    mut smp,
                    mut active_field,
                } => match key.code {
                    KeyCode::Esc => {
                        self.dialog = None;
                        self.set_status("New VM creation cancelled.", Color::DarkGray);
                    }
                    KeyCode::Tab => {
                        active_field = (active_field + 1) % 3;
                        self.dialog = Some(Dialog::NewVm {
                            name,
                            ram,
                            smp,
                            active_field,
                        });
                    }
                    KeyCode::BackTab => {
                        active_field = if active_field == 0 { 2 } else { active_field - 1 };
                        self.dialog = Some(Dialog::NewVm {
                            name,
                            ram,
                            smp,
                            active_field,
                        });
                    }
                    KeyCode::Backspace => {
                        match active_field {
                            0 => {
                                name.pop();
                            }
                            1 => {
                                ram.pop();
                            }
                            2 => {
                                smp.pop();
                            }
                            _ => {}
                        }
                        self.dialog = Some(Dialog::NewVm {
                            name,
                            ram,
                            smp,
                            active_field,
                        });
                    }
                    KeyCode::Char(c) => {
                        match active_field {
                            0 => {
                                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                                    name.push(c);
                                }
                            }
                            1 => {
                                if c.is_ascii_alphanumeric() {
                                    ram.push(c);
                                }
                            }
                            2 => {
                                if c.is_ascii_digit() {
                                    smp.push(c);
                                }
                            }
                            _ => {}
                        }
                        self.dialog = Some(Dialog::NewVm {
                            name,
                            ram,
                            smp,
                            active_field,
                        });
                    }
                    KeyCode::Enter => {
                        if name.trim().is_empty() {
                            self.set_status("VM name cannot be empty.", Color::Red);
                        } else {
                            self.dialog = None;
                            self.execute_create(name, ram, smp, action_tx);
                        }
                    }
                    _ => {}
                },
            }
            return Ok(());
        }

        // Global hotkeys
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => {
                self.should_quit = true;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_up();
                let _ = self.refresh().await;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_down();
                let _ = self.refresh().await;
            }
            KeyCode::Enter => {
                self.launch_ssh()?;
            }
            KeyCode::Char(' ') | KeyCode::Char('s') => {
                self.toggle_start_stop(action_tx);
            }
            KeyCode::Char('c') => {
                if let Some(inst) = self.selected_instance() {
                    let name = inst.name.clone();
                    let orch = self.orchestrator.clone();
                    let tag = format!(
                        "snap-{}",
                        std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs()
                            % 100000
                    );
                    let tx = action_tx.clone();
                    self.set_status(&format!("Creating snapshot '{}' for '{}'...", tag, name), Color::Yellow);
                    tokio::spawn(async move {
                        match orch.snapshot_create(&name, &tag).await {
                            Ok(_) => {
                                let _ = tx.send(ActionMessage::Status(
                                    format!("Snapshot '{}' created for '{}'!", tag, name),
                                    Color::Green,
                                ));
                            }
                            Err(e) => {
                                let _ = tx.send(ActionMessage::Status(
                                    format!("Failed to create snapshot: {:#}", e),
                                    Color::Red,
                                ));
                            }
                        }
                    });
                }
            }
            KeyCode::Char('n') => {
                let default_name = format!("vm-{}", self.instances.len() + 1);
                self.dialog = Some(Dialog::NewVm {
                    name: default_name,
                    ram: "2G".to_string(),
                    smp: "2".to_string(),
                    active_field: 0,
                });
            }
            KeyCode::Char('d') | KeyCode::Delete => {
                if let Some(inst) = self.selected_instance() {
                    let name = inst.name.clone();
                    self.dialog = Some(Dialog::ConfirmDelete { name });
                }
            }
            KeyCode::Char('r') => {
                self.refresh().await?;
                self.set_status("Instances and logs refreshed.", Color::Cyan);
            }
            KeyCode::Char('l') => {
                self.active_pane = match self.active_pane {
                    ActivePane::Instances => ActivePane::Logs,
                    ActivePane::Logs => ActivePane::Instances,
                };
            }
            KeyCode::PageUp => {
                if self.log_scroll > 5 {
                    self.log_scroll -= 5;
                } else {
                    self.log_scroll = 0;
                }
            }
            KeyCode::PageDown => {
                self.log_scroll += 5;
            }
            _ => {}
        }

        Ok(())
    }

    /// Render entire TUI frame
    pub fn render(&mut self, f: &mut Frame) {
        let size = f.area();

        // Ensure minimum reasonable dimensions
        if size.width < 50 || size.height < 14 {
            let msg = Paragraph::new("Terminal window too small for vmpod dashboard.\nPlease enlarge terminal.")
                .alignment(Alignment::Center)
                .style(Style::default().fg(Color::Yellow));
            f.render_widget(msg, size);
            return;
        }

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3), // Header banner
                Constraint::Min(9),    // Main VM pane (List + Details)
                Constraint::Length(10),// Live Logs
                Constraint::Length(1), // Bottom Status line
            ])
            .split(size);

        self.render_header(f, chunks[0]);
        self.render_body(f, chunks[1]);
        self.render_logs(f, chunks[2]);
        self.render_status(f, chunks[3]);

        // Render modal overlay if active
        if let Some(dialog) = &self.dialog {
            match dialog {
                Dialog::ConfirmDelete { name } => self.render_confirm_delete(f, size, name),
                Dialog::NewVm {
                    name,
                    ram,
                    smp,
                    active_field,
                } => self.render_new_vm_modal(f, size, name, ram, smp, *active_field),
            }
        }
    }

    fn render_header(&self, f: &mut Frame, area: Rect) {
        let hotkeys = Line::from(vec![
            Span::styled("[↑/↓] ", Style::default().fg(Color::Cyan).bold()),
            Span::raw("Move "),
            Span::styled("[Enter] ", Style::default().fg(Color::Cyan).bold()),
            Span::raw("SSH "),
            Span::styled("[Space] ", Style::default().fg(Color::Cyan).bold()),
            Span::raw("Power "),
            Span::styled("[C] ", Style::default().fg(Color::Cyan).bold()),
            Span::raw("Snap "),
            Span::styled("[N] ", Style::default().fg(Color::Cyan).bold()),
            Span::raw("New "),
            Span::styled("[D] ", Style::default().fg(Color::Cyan).bold()),
            Span::raw("Del "),
            Span::styled("[R] ", Style::default().fg(Color::Cyan).bold()),
            Span::raw("Sync "),
            Span::styled("[Q] ", Style::default().fg(Color::Cyan).bold()),
            Span::raw("Quit"),
        ]);

        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(Color::Cyan))
            .title(Span::styled(
                " ⚡ VMPOD - Hardware-Accelerated Linux VM Orchestrator ",
                Style::default().fg(Color::White).bold(),
            ));

        let paragraph = Paragraph::new(hotkeys)
            .block(block)
            .alignment(Alignment::Center);

        f.render_widget(paragraph, area);
    }

    fn render_body(&mut self, f: &mut Frame, area: Rect) {
        let body_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(40),
                Constraint::Percentage(60),
            ])
            .split(area);

        self.render_instance_list(f, body_chunks[0]);
        self.render_details(f, body_chunks[1]);
    }

    fn render_instance_list(&mut self, f: &mut Frame, area: Rect) {
        let title = format!(" Virtual Machines ({}) ", self.instances.len());
        let is_focused = self.active_pane == ActivePane::Instances;

        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(if is_focused {
                Style::default().fg(Color::Green)
            } else {
                Style::default().fg(Color::DarkGray)
            })
            .title(Span::styled(title, Style::default().bold()));

        if self.instances.is_empty() {
            let empty_text = Text::from(vec![
                Line::from(""),
                Line::from(Span::styled("No VMs found.", Style::default().fg(Color::DarkGray))),
                Line::from(""),
                Line::from(Span::styled("Press [N] to create a new VM,", Style::default().fg(Color::Cyan))),
                Line::from(Span::styled("or run 'vmpod up' in terminal.", Style::default().fg(Color::Cyan))),
            ]);
            let p = Paragraph::new(empty_text)
                .block(block)
                .alignment(Alignment::Center);
            f.render_widget(p, area);
            return;
        }

        let items: Vec<ListItem> = self
            .instances
            .iter()
            .enumerate()
            .map(|(idx, inst)| {
                let is_selected = idx == self.selected_index;

                let (dot, dot_color) = if inst.running {
                    ("●", Color::Green)
                } else {
                    ("○", Color::Red)
                };

                let port_label = if let Some(p) = inst.port {
                    format!(":{}", p)
                } else {
                    "off".to_string()
                };

                let line = Line::from(vec![
                    Span::styled(format!(" {} ", dot), Style::default().fg(dot_color).bold()),
                    Span::styled(
                        format!("{:<14}", inst.name),
                        if is_selected {
                            Style::default().bold().fg(Color::White)
                        } else {
                            Style::default().fg(Color::White)
                        },
                    ),
                    Span::styled(
                        format!(" [{}]", port_label),
                        if inst.running {
                            Style::default().fg(Color::Cyan)
                        } else {
                            Style::default().fg(Color::DarkGray)
                        },
                    ),
                ]);

                ListItem::new(line)
            })
            .collect();

        let list = List::new(items)
            .block(block)
            .highlight_style(
                Style::default()
                    .bg(Color::Rgb(30, 60, 110))
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("▶ ");

        f.render_stateful_widget(list, area, &mut self.list_state);
    }

    fn render_details(&self, f: &mut Frame, area: Rect) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(Color::DarkGray))
            .title(Span::styled(" VM Instance Details ", Style::default().bold()));

        let Some(inst) = self.selected_instance() else {
            let p = Paragraph::new(Line::from(Span::styled(
                "Select a virtual machine from the left panel.",
                Style::default().fg(Color::DarkGray),
            )))
            .block(block)
            .alignment(Alignment::Center);
            f.render_widget(p, area);
            return;
        };

        let mut lines = Vec::new();

        // 1. Name & Status Banner
        let status_span = if inst.running {
            Span::styled(" ● RUNNING ", Style::default().bg(Color::Green).fg(Color::Black).bold())
        } else {
            Span::styled(" ○ STOPPED ", Style::default().bg(Color::Red).fg(Color::White).bold())
        };

        lines.push(Line::from(vec![
            Span::styled(format!(" {} ", inst.name), Style::default().fg(Color::White).bold()),
            status_span,
            Span::raw("   "),
            Span::styled(format!("Uptime: {}", inst.uptime), Style::default().fg(Color::DarkGray)),
        ]));
        lines.push(Line::from(""));

        // 2. Hardware specs
        lines.push(Line::from(vec![
            Span::styled("  vCPUs:         ", Style::default().fg(Color::DarkGray)),
            Span::styled(format!("{} cores", inst.smp), Style::default().fg(Color::Yellow).bold()),
            Span::styled("    Memory:  ", Style::default().fg(Color::DarkGray)),
            Span::styled(&inst.ram, Style::default().fg(Color::Yellow).bold()),
        ]));

        // 3. Network & SSH
        let port_str = inst.port.map(|p| p.to_string()).unwrap_or_else(|| "None".to_string());
        lines.push(Line::from(vec![
            Span::styled("  SSH Port:      ", Style::default().fg(Color::DarkGray)),
            Span::styled(format!("localhost:{}", port_str), Style::default().fg(Color::Cyan).bold()),
        ]));

        // 4. Container ID
        lines.push(Line::from(vec![
            Span::styled("  Container ID:  ", Style::default().fg(Color::DarkGray)),
            Span::styled(&inst.short_id, Style::default().fg(Color::White)),
        ]));

        // 5. Storage Path & Disk file
        lines.push(Line::from(vec![
            Span::styled("  Storage Path:  ", Style::default().fg(Color::DarkGray)),
            Span::styled(&inst.storage_path, Style::default().fg(Color::White)),
        ]));

        // Disk size on host check
        let disk_file = format!("{}/disk.qcow2", inst.storage_path);
        let disk_size_str = if let Ok(meta) = std::fs::metadata(&disk_file) {
            let bytes = meta.len();
            if bytes >= 1024 * 1024 * 1024 {
                format!("{:.2} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
            } else if bytes >= 1024 * 1024 {
                format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
            } else {
                format!("{} KB", bytes / 1024)
            }
        } else {
            "Not initialized".to_string()
        };

        lines.push(Line::from(vec![
            Span::styled("  Virtual Disk:  ", Style::default().fg(Color::DarkGray)),
            Span::styled(disk_size_str, Style::default().fg(Color::White)),
            Span::styled(" (differential qcow2 overlay)", Style::default().fg(Color::DarkGray)),
        ]));

        lines.push(Line::from(""));

        // 6. Quick SSH Connect Helper
        if inst.running {
            let key_path = format!("{}/id_ed25519", inst.storage_path);
            let ssh_hint = if Path::new(&key_path).exists() {
                format!("ssh -i {} root@localhost -p {}", key_path, port_str)
            } else {
                format!("ssh root@localhost -p {}", port_str)
            };

            lines.push(Line::from(vec![
                Span::styled("  SSH Command:   ", Style::default().fg(Color::DarkGray)),
                Span::styled(ssh_hint, Style::default().fg(Color::LightGreen)),
            ]));
            lines.push(Line::from(vec![
                Span::raw("                 "),
                Span::styled("(Press [Enter] to launch an interactive SSH shell)", Style::default().fg(Color::DarkGray).italic()),
            ]));
        } else {
            lines.push(Line::from(vec![
                Span::styled("  Action:        ", Style::default().fg(Color::DarkGray)),
                Span::styled("Press [Space] to power on this VM.", Style::default().fg(Color::Yellow)),
            ]));
        }

        let p = Paragraph::new(lines).block(block);
        f.render_widget(p, area);
    }

    fn render_logs(&self, f: &mut Frame, area: Rect) {
        let is_focused = self.active_pane == ActivePane::Logs;

        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(if is_focused {
                Style::default().fg(Color::Green)
            } else {
                Style::default().fg(Color::DarkGray)
            })
            .title(Span::styled(
                " 📜 Guest Kernel & Runner Console Logs ",
                Style::default().bold(),
            ));

        let lines: Vec<Line> = self
            .logs
            .iter()
            .map(|l| {
                let trimmed = l.trim_end();
                if trimmed.contains("[ ok ]") || trimmed.contains("done") {
                    Line::from(vec![
                        Span::styled(trimmed, Style::default().fg(Color::LightGreen)),
                    ])
                } else if trimmed.contains("error") || trimmed.contains("Error") || trimmed.contains("failed") {
                    Line::from(vec![
                        Span::styled(trimmed, Style::default().fg(Color::LightRed)),
                    ])
                } else if trimmed.contains("Tiny Cloud") || trimmed.contains("Linux version") {
                    Line::from(vec![
                        Span::styled(trimmed, Style::default().fg(Color::Cyan).bold()),
                    ])
                } else {
                    Line::from(vec![
                        Span::styled(trimmed, Style::default().fg(Color::Gray)),
                    ])
                }
            })
            .collect();

        let visible_lines = area.height.saturating_sub(2) as usize;
        let total_lines = lines.len();
        let scroll_offset = if total_lines > visible_lines {
            (total_lines - visible_lines).saturating_sub(self.log_scroll) as u16
        } else {
            0
        };

        let p = Paragraph::new(lines)
            .block(block)
            .scroll((scroll_offset, 0))
            .wrap(Wrap { trim: false });

        f.render_widget(p, area);
    }

    fn render_status(&self, f: &mut Frame, area: Rect) {
        let (msg, color) = if let Some((ref text, time, col)) = self.status_message {
            if time.elapsed() < Duration::from_secs(5) {
                (text.as_str(), col)
            } else {
                ("Ready.", Color::DarkGray)
            }
        } else {
            ("Ready.", Color::DarkGray)
        };

        let running_count = self.instances.iter().filter(|i| i.running).count();
        let stopped_count = self.instances.len().saturating_sub(running_count);

        let status_line = Line::from(vec![
            Span::styled(" ", Style::default()),
            Span::styled(msg, Style::default().fg(color).bold()),
            Span::styled(
                format!(
                    "  │  {} running, {} stopped  │  vmpod v1.2.0 ",
                    running_count, stopped_count
                ),
                Style::default().fg(Color::DarkGray),
            ),
        ]);

        let p = Paragraph::new(status_line).alignment(Alignment::Left);
        f.render_widget(p, area);
    }

    fn render_confirm_delete(&self, f: &mut Frame, area: Rect, name: &str) {
        let popup_area = popup_rect(60, 9, area);

        f.render_widget(Clear, popup_area);

        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Double)
            .border_style(Style::default().fg(Color::Red).bold())
            .title(Span::styled(" Confirm Deletion ", Style::default().fg(Color::Red).bold()));

        let text = Text::from(vec![
            Line::from(""),
            Line::from(vec![
                Span::raw("Are you sure you want to delete VM '"),
                Span::styled(name, Style::default().fg(Color::Yellow).bold()),
                Span::raw("' ?"),
            ]),
            Line::from(""),
            Line::from(Span::styled(
                "This will gracefully stop the VM and purge its virtual disk.",
                Style::default().fg(Color::DarkGray),
            )),
            Line::from(""),
            Line::from(vec![
                Span::styled(" [Y] Yes, Delete ", Style::default().bg(Color::Red).fg(Color::White).bold()),
                Span::raw("    "),
                Span::styled(" [N] Cancel [Esc] ", Style::default().bg(Color::DarkGray).fg(Color::White)),
            ]),
        ]);

        let p = Paragraph::new(text)
            .block(block)
            .alignment(Alignment::Center);

        f.render_widget(p, popup_area);
    }

    fn render_new_vm_modal(
        &self,
        f: &mut Frame,
        area: Rect,
        name: &str,
        ram: &str,
        smp: &str,
        active_field: usize,
    ) {
        let popup_area = popup_rect(54, 12, area);

        f.render_widget(Clear, popup_area);

        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(Color::Cyan).bold())
            .title(Span::styled(" 🚀 Create New Virtual Machine ", Style::default().bold()));

        let field_style = |field_idx| {
            if active_field == field_idx {
                Style::default().fg(Color::Yellow).bold()
            } else {
                Style::default().fg(Color::White)
            }
        };

        let pointer = |field_idx| {
            if active_field == field_idx {
                "▶ "
            } else {
                "  "
            }
        };

        let text = Text::from(vec![
            Line::from(""),
            Line::from(vec![
                Span::styled(pointer(0), Style::default().fg(Color::Yellow)),
                Span::styled("Instance Name: ", field_style(0)),
                Span::styled(format!("[ {:<18} ]", name), field_style(0)),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::styled(pointer(1), Style::default().fg(Color::Yellow)),
                Span::styled("RAM Size:      ", field_style(1)),
                Span::styled(format!("[ {:<18} ]", ram), field_style(1)),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::styled(pointer(2), Style::default().fg(Color::Yellow)),
                Span::styled("vCPU Cores:    ", field_style(2)),
                Span::styled(format!("[ {:<18} ]", smp), field_style(2)),
            ]),
            Line::from(""),
            Line::from(Span::styled(
                "[Tab] Switch Field  │  [Enter] Launch  │  [Esc] Cancel",
                Style::default().fg(Color::DarkGray),
            )),
        ]);

        let p = Paragraph::new(text)
            .block(block)
            .alignment(Alignment::Center);

        f.render_widget(p, popup_area);
    }
}

/// Helper function to create a centered popup rect with bounded width and height
fn popup_rect(width: u16, height: u16, r: Rect) -> Rect {
    let w = width.min(r.width.saturating_sub(4));
    let h = height.min(r.height.saturating_sub(2));
    let x = r.x + (r.width.saturating_sub(w)) / 2;
    let y = r.y + (r.height.saturating_sub(h)) / 2;
    Rect {
        x,
        y,
        width: w,
        height: h,
    }
}

/// Run the interactive dashboard
pub async fn run_dashboard(orchestrator: Arc<VmOrchestrator>) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let (action_tx, mut action_rx) = tokio::sync::mpsc::unbounded_channel::<ActionMessage>();

    let mut app = App::new(orchestrator);
    let _ = app.refresh().await;

    // Set panic hook so terminal is always restored cleanly if an error occurs
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        original_hook(panic_info);
    }));

    let tick_rate = Duration::from_millis(50); // 20 FPS smooth event loop
    let mut last_tick = Instant::now();

    while !app.should_quit {
        // Drain any messages from background tasks
        let mut needs_refresh = false;
        while let Ok(msg) = action_rx.try_recv() {
            match msg {
                ActionMessage::Status(text, color) => {
                    app.set_status(text, color);
                }
                ActionMessage::Refresh => {
                    needs_refresh = true;
                }
            }
        }

        if needs_refresh {
            let _ = app.refresh().await;
        }

        terminal.draw(|f| app.render(f))?;

        let timeout = tick_rate
            .checked_sub(last_tick.elapsed())
            .unwrap_or_else(|| Duration::from_millis(0));

        if event::poll(timeout)? {
            match event::read()? {
                Event::Key(key) => {
                    if key.code == KeyCode::Enter && app.dialog.is_none() {
                        let _ = disable_raw_mode();
                        let _ = execute!(terminal.backend_mut(), LeaveAlternateScreen);
                        app.launch_ssh()?;
                        let _ = execute!(terminal.backend_mut(), EnterAlternateScreen);
                        let _ = enable_raw_mode();
                        terminal.clear()?;
                        let _ = app.refresh().await;
                    } else {
                        app.handle_key(key, &action_tx).await?;
                    }
                }
                Event::Resize(_, _) => {
                    terminal.clear()?;
                }
                _ => {}
            }
        }

        if last_tick.elapsed() >= tick_rate {
            // Background periodic refresh every 3 seconds
            if app.last_refresh.elapsed() >= Duration::from_secs(3) && app.dialog.is_none() {
                let _ = app.refresh().await;
            }
            last_tick = Instant::now();
        }
    }

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;

    fn create_mock_app() -> App {
        let orchestrator = Arc::new(VmOrchestrator::new().unwrap());
        App::new(orchestrator)
    }

    #[test]
    fn test_render_empty_state_80x24() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = create_mock_app();

        terminal.draw(|f| app.render(f)).unwrap();

        let buffer = terminal.backend().buffer().clone();
        let rendered_text = format!("{:?}", buffer);

        // Header and border checks
        assert!(rendered_text.contains("VMPOD"));
        assert!(rendered_text.contains("Virtual Machines (0)"));
        assert!(rendered_text.contains("No VMs found"));
        assert!(rendered_text.contains("Press [N] to create a new VM"));
        assert!(rendered_text.contains("Dashboard loaded") || rendered_text.contains("Ready."));
    }

    #[test]
    fn test_render_with_instances_and_details() {
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = create_mock_app();

        app.instances = vec![
            InstanceSummary {
                name: "prod-vm".to_string(),
                container_id: "a1b2c3d4e5f6".to_string(),
                short_id: "a1b2c3d4e5f6".to_string(),
                status: "running".to_string(),
                running: true,
                port: Some(2222),
                ssh_command: "ssh root@localhost -p 2222".to_string(),
                uptime: "2h 15m".to_string(),
                ram: "4G".to_string(),
                smp: "4".to_string(),
                storage_path: "/home/kevin/vmpod-data/prod-vm".to_string(),
                ephemeral: false,
            },
            InstanceSummary {
                name: "backup-vm".to_string(),
                container_id: "b2c3d4e5f6a1".to_string(),
                short_id: "b2c3d4e5f6a1".to_string(),
                status: "stopped".to_string(),
                running: false,
                port: None,
                ssh_command: "vmpod up backup-vm".to_string(),
                uptime: "offline".to_string(),
                ram: "2G".to_string(),
                smp: "2".to_string(),
                storage_path: "/home/kevin/vmpod-data/backup-vm".to_string(),
                ephemeral: false,
            },
        ];
        app.selected_index = 0;
        app.list_state.select(Some(0));
        app.logs = vec![
            "Linux version 6.6.121-0-virt (Alpine Linux)".to_string(),
            "Tiny Cloud: bootstrap complete.".to_string(),
        ];

        terminal.draw(|f| app.render(f)).unwrap();

        let buffer = terminal.backend().buffer().clone();
        let rendered_text = format!("{:?}", buffer);

        assert!(rendered_text.contains("prod-vm"));
        assert!(rendered_text.contains("backup-vm"));
        assert!(rendered_text.contains("RUNNING"));
        assert!(rendered_text.contains("4 cores"));
        assert!(rendered_text.contains("4G"));
        assert!(rendered_text.contains("localhost:2222"));
        assert!(rendered_text.contains("Tiny Cloud: bootstrap complete"));
    }

    #[test]
    fn test_navigation_wraparound() {
        let mut app = create_mock_app();
        app.instances = vec![
            InstanceSummary {
                name: "vm-1".to_string(),
                container_id: "1".to_string(),
                short_id: "1".to_string(),
                status: "running".to_string(),
                running: true,
                port: Some(2222),
                ssh_command: "".to_string(),
                uptime: "1m".to_string(),
                ram: "2G".to_string(),
                smp: "2".to_string(),
                storage_path: "".to_string(),
                ephemeral: false,
            },
            InstanceSummary {
                name: "vm-2".to_string(),
                container_id: "2".to_string(),
                short_id: "2".to_string(),
                status: "running".to_string(),
                running: true,
                port: Some(2223),
                ssh_command: "".to_string(),
                uptime: "1m".to_string(),
                ram: "2G".to_string(),
                smp: "2".to_string(),
                storage_path: "".to_string(),
                ephemeral: false,
            },
        ];

        assert_eq!(app.selected_index, 0);
        app.move_down();
        assert_eq!(app.selected_index, 1);
        app.move_down(); // Wrap around to top
        assert_eq!(app.selected_index, 0);
        app.move_up(); // Wrap around to bottom
        assert_eq!(app.selected_index, 1);
    }

    #[test]
    fn test_render_confirm_delete_modal_alignment() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = create_mock_app();

        app.dialog = Some(Dialog::ConfirmDelete {
            name: "test-node".to_string(),
        });

        terminal.draw(|f| app.render(f)).unwrap();

        let buffer = terminal.backend().buffer().clone();
        let rendered_text = format!("{:?}", buffer);

        assert!(rendered_text.contains("Confirm Deletion"));
        assert!(rendered_text.contains("test-node"));
        assert!(rendered_text.contains("[Y] Yes, Delete"));
    }

    #[test]
    fn test_render_new_vm_modal_alignment() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = create_mock_app();

        app.dialog = Some(Dialog::NewVm {
            name: "cluster-worker".to_string(),
            ram: "4G".to_string(),
            smp: "4".to_string(),
            active_field: 0,
        });

        terminal.draw(|f| app.render(f)).unwrap();

        let buffer = terminal.backend().buffer().clone();
        let rendered_text = format!("{:?}", buffer);

        assert!(rendered_text.contains("Create New Virtual Machine"));
        assert!(rendered_text.contains("cluster-worker"));
        assert!(rendered_text.contains("4G"));
        assert!(rendered_text.contains("4"));
    }

    #[test]
    fn test_small_terminal_fallback() {
        let backend = TestBackend::new(40, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = create_mock_app();

        terminal.draw(|f| app.render(f)).unwrap();

        let buffer = terminal.backend().buffer().clone();
        let rendered_text = format!("{:?}", buffer);

        assert!(rendered_text.contains("Terminal window too small"));
    }

    #[test]
    fn test_responsive_layout_dimensions() {
        let sizes = [(60, 16), (80, 24), (120, 35), (160, 50)];

        for (w, h) in sizes {
            let backend = TestBackend::new(w, h);
            let mut terminal = Terminal::new(backend).unwrap();
            let mut app = create_mock_app();
            app.instances = vec![InstanceSummary {
                name: "node-alpha".to_string(),
                container_id: "c1".to_string(),
                short_id: "c1".to_string(),
                status: "running".to_string(),
                running: true,
                port: Some(2222),
                ssh_command: "".to_string(),
                uptime: "10m".to_string(),
                ram: "2G".to_string(),
                smp: "2".to_string(),
                storage_path: "/data".to_string(),
                ephemeral: false,
            }];

            // Drawing must succeed without panic on any of these resolutions
            terminal.draw(|f| app.render(f)).unwrap();

            let buffer = terminal.backend().buffer().clone();
            let rendered_text = format!("{:?}", buffer);
            assert!(rendered_text.contains("node-alpha"));
        }
    }

    #[tokio::test]
    async fn test_new_vm_modal_form_inputs() {
        let mut app = create_mock_app();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        app.dialog = Some(Dialog::NewVm {
            name: "init".to_string(),
            ram: "1G".to_string(),
            smp: "1".to_string(),
            active_field: 0,
        });

        // Type 'a' into name
        app.handle_key(KeyEvent::new(KeyCode::Char('a'), crossterm::event::KeyModifiers::NONE), &tx)
            .await
            .unwrap();

        match &app.dialog {
            Some(Dialog::NewVm { name, .. }) => assert_eq!(name, "inita"),
            _ => panic!("Expected NewVm dialog"),
        }

        // Backspace
        app.handle_key(KeyEvent::new(KeyCode::Backspace, crossterm::event::KeyModifiers::NONE), &tx)
            .await
            .unwrap();
        match &app.dialog {
            Some(Dialog::NewVm { name, .. }) => assert_eq!(name, "init"),
            _ => panic!("Expected NewVm dialog"),
        }

        // Tab to RAM field (index 1)
        app.handle_key(KeyEvent::new(KeyCode::Tab, crossterm::event::KeyModifiers::NONE), &tx)
            .await
            .unwrap();
        match &app.dialog {
            Some(Dialog::NewVm { active_field, .. }) => assert_eq!(*active_field, 1),
            _ => panic!("Expected NewVm dialog"),
        }
    }

    #[tokio::test]
    async fn test_confirm_delete_cancellation() {
        let mut app = create_mock_app();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        app.dialog = Some(Dialog::ConfirmDelete {
            name: "cancel-me".to_string(),
        });

        // Press 'n' to cancel
        app.handle_key(KeyEvent::new(KeyCode::Char('n'), crossterm::event::KeyModifiers::NONE), &tx)
            .await
            .unwrap();

        assert!(app.dialog.is_none());
        assert!(app.status_message.unwrap().0.contains("Deletion cancelled"));
    }

    #[test]
    fn test_visual_rendered_screen_80x24() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = create_mock_app();
        app.instances = vec![
            InstanceSummary {
                name: "cluster-1".to_string(),
                container_id: "c11122233344".to_string(),
                short_id: "c11122233344".to_string(),
                status: "running".to_string(),
                running: true,
                port: Some(2222),
                ssh_command: "ssh root@localhost -p 2222".to_string(),
                uptime: "14m 20s".to_string(),
                ram: "2G".to_string(),
                smp: "2".to_string(),
                storage_path: "/home/kevin/vmpod-data/cluster-1".to_string(),
                ephemeral: false,
            },
            InstanceSummary {
                name: "cluster-2".to_string(),
                container_id: "offline".to_string(),
                short_id: "offline".to_string(),
                status: "stopped".to_string(),
                running: false,
                port: None,
                ssh_command: "vmpod up cluster-2".to_string(),
                uptime: "offline".to_string(),
                ram: "-".to_string(),
                smp: "-".to_string(),
                storage_path: "/home/kevin/vmpod-data/cluster-2".to_string(),
                ephemeral: false,
            },
        ];
        app.selected_index = 0;
        app.list_state.select(Some(0));
        app.logs = vec![
            "[ 0.000000] Linux version 6.6.121-0-virt (Alpine Linux)".to_string(),
            "[ 1.204512] eth0: leased 10.0.2.15 for 86400 seconds [ ok ]".to_string(),
            "[ 2.112004] Tiny Cloud: bootstrap complete.".to_string(),
        ];

        terminal.draw(|f| app.render(f)).unwrap();

        let buffer = terminal.backend().buffer();
        println!("\n--- RENDERED 80x24 TERMINAL VIEW ---");
        for y in 0..24 {
            let mut line = String::new();
            for x in 0..80 {
                line.push(buffer[(x, y)].symbol().chars().next().unwrap_or(' '));
            }
            println!("{}", line);
        }
        println!("--- END RENDERED VIEW ---\n");
    }
}
