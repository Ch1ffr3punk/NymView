use nym_sdk::mixnet;
use nym_sdk::mixnet::MixnetMessageSender;
use egui::{Ui, TextEdit, ScrollArea, Color32};
use tokio::sync::mpsc;
use once_cell::sync::Lazy;
use tokio::runtime::Runtime;
use std::sync::{Arc, Mutex};
use std::sync::OnceLock;
use egui_commonmark::{CommonMarkCache, CommonMarkViewer};
use eframe::App;
use std::time::{Duration, Instant};

// Global runtime for async operations
static RUNTIME: Lazy<Runtime> = Lazy::new(|| {
    Runtime::new().expect("Failed to create Tokio runtime")
});

// Global sender for communication between GUI and mixnet
static GUI_TO_MIXNET_SENDER: OnceLock<Arc<Mutex<Option<mpsc::UnboundedSender<BrowserMessage>>>>> =
    OnceLock::new();

#[derive(Debug)]
pub(crate) enum BrowserMessage {
    SendRequest { recipient: String, message: String },
    ReceivedMessage { content: String},
    ConnectionStatus { status: String, loading: bool, client_address: String },
}

#[derive(Debug, Clone)]
pub(crate) struct HistoryEntry {
    pub server: String,
    pub page: String,
    pub content: String,
    #[allow(dead_code)]
    pub timestamp: Instant,
}

pub struct NymMixnetBrowser {
    pub address_bar: String,
    pub current_content: String,
    pub loading: bool,
    pub page_loading: bool,
    pub error: Option<String>,
    pub connection_status: String,
    pub server_address: String,
    pub client_address: String,
    pub(crate) message_receiver: Option<mpsc::UnboundedReceiver<BrowserMessage>>,
    pub(crate) message_sender: Option<mpsc::UnboundedSender<BrowserMessage>>,
    pub(crate) history: Vec<HistoryEntry>,
    pub(crate) connection_attempted: bool,
    pub(crate) md_cache: CommonMarkCache,
    pub(crate) pending_navigation: Option<String>,
    pub(crate) current_history_index: usize,
    pub(crate) page_load_start_time: Option<Instant>,
}

impl NymMixnetBrowser {
    pub fn new() -> Self {
        Self {
            address_bar: String::new(),
            current_content: String::new(),
            loading: true,
            page_loading: false,
            error: None,
            connection_status: "Connecting to Mixnet...".to_string(),
            server_address: String::new(),
            client_address: String::new(),
            message_receiver: None,
            message_sender: None,
            history: Vec::new(),
            connection_attempted: false,
            md_cache: CommonMarkCache::default(),
            pending_navigation: None,
            current_history_index: 0,
            page_load_start_time: None,
        }
    }

    pub fn init(&mut self) {
        if !self.connection_attempted {
            let (tx, rx) = mpsc::unbounded_channel::<BrowserMessage>();
            self.message_sender = Some(tx);
            self.message_receiver = Some(rx);
            self.connection_attempted = true;
            self.start_connection();
        }
    }

    fn start_connection(&mut self) {
        if let Some(sender) = self.message_sender.clone() {
            RUNTIME.spawn(async move {
                match Self::connect_with_status(sender).await {
                    Ok(_) => {},
                    Err(e) => eprintln!("Connection failed: {}", e),
                }
            });
        }
    }

    async fn connect_with_status(sender: mpsc::UnboundedSender<BrowserMessage>) -> Result<(), String> {
        let _ = sender.send(BrowserMessage::ConnectionStatus {
            status: "Connecting to Mixnet...".to_string(),
            loading: true,
            client_address: String::new(),
        });

        let client = mixnet::MixnetClientBuilder::new_ephemeral()
            .build()
            .map_err(|e| format!("Client creation error: {}", e))?;

        let connected_client = client
            .connect_to_mixnet()
            .await
            .map_err(|e| format!("Connection error: {}", e))?;

        let client_address = connected_client.nym_address().to_string();

        let _ = sender.send(BrowserMessage::ConnectionStatus {
            status: "Connected".to_string(),
            loading: false,
            client_address: client_address.clone(),
        });

        GUI_TO_MIXNET_SENDER.get_or_init(|| Arc::new(Mutex::new(None)));
        let (gui_to_mixnet_tx, gui_to_mixnet_rx) = mpsc::unbounded_channel::<BrowserMessage>();
        *GUI_TO_MIXNET_SENDER.get().unwrap().lock().unwrap() = Some(gui_to_mixnet_tx);

        RUNTIME.spawn(Self::mixnet_task(
            connected_client,
            gui_to_mixnet_rx,
            sender,
        ));

        Ok(())
    }

    async fn mixnet_task(
        mut client: mixnet::MixnetClient,
        mut from_gui: mpsc::UnboundedReceiver<BrowserMessage>,
        to_gui: mpsc::UnboundedSender<BrowserMessage>,
    ) {
        loop {
            tokio::select! {
                messages = client.wait_for_messages() => {
                    if let Some(messages) = messages {
                        for received in messages {
                            let text_message = String::from_utf8_lossy(&received.message).into_owned();
                            let _sender_info = if let Some(sender_tag) = &received.sender_tag {
                                format!("{:?}", sender_tag)
                            } else {
                                "unknown".to_string()
                            };
                            let _ = to_gui.send(BrowserMessage::ReceivedMessage {
                                content: text_message,
                            });
                        }
                    }
                }
                Some(gui_message) = from_gui.recv() => {
                    if let BrowserMessage::SendRequest { recipient, message } = gui_message {
                        match recipient.parse::<nym_sdk::mixnet::Recipient>() {
                            Ok(recipient_addr) => {
                                if let Err(e) = client.send_plain_message(recipient_addr, message).await {
                                    let _ = to_gui.send(BrowserMessage::ReceivedMessage {
                                        content: format!("ERROR: {}", e),
                                    });
                                }
                            }
                            Err(e) => {
                                let _ = to_gui.send(BrowserMessage::ReceivedMessage {
                                    content: format!("ERROR: Invalid address - {}", e),
                                });
                            }
                        }
                    }
                }
            }
        }
    }

    fn get_gui_sender() -> Option<mpsc::UnboundedSender<BrowserMessage>> {
        GUI_TO_MIXNET_SENDER
            .get()
            .and_then(|arc| arc.lock().unwrap().clone())
    }

    pub fn send_request(&self, request_path: &str) -> Result<(), String> {
        let recipient = self.server_address.trim();
        if recipient.is_empty() {
            return Err("No server address specified".to_string());
        }
        
        let my_address = self.client_address.trim();
        if my_address.is_empty() {
            return Err("Not connected yet - waiting for client address".to_string());
        }

        let request = format!("GET {} FROM {}", request_path, my_address);

        if let Some(sender) = Self::get_gui_sender() {
            sender.send(BrowserMessage::SendRequest {
                recipient: recipient.to_string(),
                message: request,
            }).map_err(|e| format!("Send error: {}", e))?;
        } else {
            return Err("Not connected to Mixnet".to_string());
        }
        Ok(())
    }

    fn parse_and_set_url(&mut self, url: &str) {
        if let Some((server, page)) = Self::parse_nym_url(url) {
            self.server_address = server.trim().to_string();
            self.address_bar = if page.is_empty() { String::new() } else { page };
        } else {
            self.address_bar = url.trim().to_string();
        }
    }

    fn parse_nym_url(url: &str) -> Option<(String, String)> {
        if !url.starts_with("nym://") {
            return None;
        }
        let without_protocol = &url[6..];
        if let Some(slash_pos) = without_protocol.find('/') {
            let server = without_protocol[..slash_pos].to_string();
            let page = without_protocol.get(slash_pos + 1..).unwrap_or("").to_string();
            Some((server, page))
        } else {
            Some((without_protocol.to_string(), "".to_string()))
        }
    }

    fn handle_navigation(&mut self) {
        let address = self.address_bar.clone();
        self.parse_and_set_url(&address);
        self.page_loading = true;
        self.page_load_start_time = Some(Instant::now());

        let path = if self.address_bar.is_empty() {
            "/".to_string()
        } else if self.address_bar.starts_with('/') {
            self.address_bar.clone()
        } else {
            format!("/{}", self.address_bar)
        };

        match self.send_request(&path) {
            Ok(()) => {
                self.add_to_history();
            },
            Err(e) => {
                self.error = Some(e);
                self.page_loading = false;
                self.page_load_start_time = None;
            }
        }
    }

    pub fn show(&mut self, ui: &mut Ui) {
        ui.style_mut().url_in_tooltip = true;

        if !self.connection_attempted {
            self.init();
        }

        // Check for page load timeout (30 seconds)
        if self.page_loading {
            if let Some(start_time) = self.page_load_start_time {
                if start_time.elapsed() > Duration::from_secs(30) {
                    self.error = Some("Page load timeout - server not responding".to_string());
                    self.page_loading = false;
                    self.page_load_start_time = None;
                }
            }
        }

        // Process pending navigation first
        if let Some(url) = self.pending_navigation.take() {
            self.handle_link_click(&url);
        }

        let mut messages_to_process = Vec::new();
        if let Some(receiver) = &mut self.message_receiver {
            while let Ok(message) = receiver.try_recv() {
                messages_to_process.push(message);
            }
        }

        for message in messages_to_process {
            match message {
                BrowserMessage::ReceivedMessage { content} => {
                    self.handle_server_message(content);
                }
                BrowserMessage::ConnectionStatus { status, loading, client_address } => {
                    self.connection_status = status;
                    self.loading = loading;
                    if !client_address.is_empty() {
                        self.client_address = client_address;
                    }
                }
                _ => {}
            }
        }

        // Status line
        ui.horizontal(|ui| {
            ui.label("Status:");
            ui.colored_label(Color32::BLUE, &self.connection_status);
            if self.loading {
                ui.spinner();
                ui.colored_label(Color32::BLUE, "Connecting...");
            }
        });

        ui.separator();

        // Address bar with navigation buttons
        ui.horizontal(|ui| {
            // Navigation buttons with tooltips
            let can_go_back = self.current_history_index > 0;
            let can_go_forward = self.current_history_index < self.history.len().saturating_sub(1);
            
            if ui.add_enabled(can_go_back, egui::Button::new("◀"))
                .on_hover_text("Go back")
                .clicked() {
                self.go_back();
            }
            
            if ui.add_enabled(can_go_forward, egui::Button::new("▶"))
                .on_hover_text("Go forward") 
                .clicked() {
                self.go_forward();
            }
            
            if ui.button("🔄")
                .on_hover_text("Reload page")
                .clicked() {
                self.reload_current_page();
            }
            
            ui.label("Address:");
            
            // Address text field
            let available_width = ui.available_width();
            let text_width = available_width - 40.0;
            let response = ui.add(
                TextEdit::singleline(&mut self.address_bar)
                    .hint_text("nym://server/page")
                    .desired_width(text_width)
                    .min_size(egui::Vec2::new(300.0, 0.0))
            );

            let can_navigate = !self.loading && !self.address_bar.trim().is_empty();
            
            // Right-aligned buttons
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Go").clicked() && can_navigate {
                    self.handle_navigation();
                }
                
                //if self.page_loading {
                    //ui.spinner();
                    //if let Some(start_time) = self.page_load_start_time {
                        //let elapsed = start_time.elapsed();
                        //if elapsed > Duration::from_secs(2) {
                            //ui.colored_label(Color32::BLUE, format!("{:.1}s", elapsed.as_secs_f32()));
                        //}
                    //}
                //}
            });

            // Enter key handling
            if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && can_navigate {
                self.handle_navigation();
            }
        });

        if let Some(ref err) = self.error {
            ui.colored_label(Color32::RED, err);
        }

        ScrollArea::vertical().show(ui, |ui| {
            if self.page_loading {
                ui.vertical_centered(|ui| {
                    ui.spinner();
                    // ui.label("Loading via Mixnet...");
                    if let Some(start_time) = self.page_load_start_time {
                        let elapsed = start_time.elapsed();
                        ui.colored_label(Color32::BLUE, format!("Loading for {:.1} seconds", elapsed.as_secs_f32()));
                    }
                });
            } else if self.current_content.is_empty() {
                self.show_welcome_page(ui);
            } else {
                // Simple approach: render markdown directly
                CommonMarkViewer::new()
                    .show(ui, &mut self.md_cache, &self.current_content);
                
                // Simple link detection - just detect clicks anywhere in content
                let response = ui.allocate_rect(ui.max_rect(), egui::Sense::click());
                
                if response.clicked() {
                    // Extract first nym:// link from content as fallback
                    let links = Self::extract_nym_links(&self.current_content);
                    if !links.is_empty() {
                        self.pending_navigation = Some(links[0].clone());
                    }
                }
            }
        });
    }

    // Extract all nym:// links from content
    fn extract_nym_links(content: &str) -> Vec<String> {
        let mut links = Vec::new();
        let mut search_pos = 0;
        
        while let Some(start) = content[search_pos..].find("nym://") {
            let actual_start = search_pos + start;
            let remaining = &content[actual_start..];
            
            let end = remaining.find(|c: char| c.is_whitespace() || c == ')' || c == ']' || c == '>' || c == '"' || c == '\'')
                .unwrap_or(remaining.len());
                
            let link = &remaining[..end];
            if !link.is_empty() && !links.contains(&link.to_string()) {
                links.push(link.to_string());
            }
            
            search_pos = actual_start + end;
            if search_pos >= content.len() {
                break;
            }
        }
        
        links
    }

    // Handle link clicks - SIMPLIFIED AND STABLE VERSION
    fn handle_link_click(&mut self, href: &str) {
        if href.starts_with("nym://") {
            if let Some((server, page)) = Self::parse_nym_url(href) {
                // Simple logic: if it looks like a Nym address, treat as external
                if server.contains('.') && server.contains('@') {
                    // External link
                    let old_server = self.server_address.clone();
                    self.server_address = server.clone();
                    
                    let path = if page.is_empty() { 
                        "/".to_string() 
                    } else { 
                        format!("/{}", page) 
                    };
                    
                    self.address_bar = if page.is_empty() { String::new() } else { page };
                    
                    self.page_loading = true;
                    self.page_load_start_time = Some(Instant::now());
                    
                    if let Err(e) = self.send_request(&path) {
                        self.server_address = old_server;
                        self.error = Some(e);
                        self.page_loading = false;
                        self.page_load_start_time = None;
                    } else {
                        self.add_to_history();
                    }
                } else {
                    // Local link
                    if !self.server_address.is_empty() {
                        let path = if page.is_empty() { 
                            format!("/{}", server) 
                        } else { 
                            format!("/{}/{}", server, page) 
                        };
                        
                        self.page_loading = true;
                        self.page_load_start_time = Some(Instant::now());
                        
                        if let Err(e) = self.send_request(&path) {
                            self.error = Some(e);
                            self.page_loading = false;
                            self.page_load_start_time = None;
                        } else {
                            self.add_to_history();
                        }
                    }
                }
            }
        } else if href.starts_with('/') {
            let path = &href[1..];
            self.navigate_to(path);
        } else {
            self.navigate_to(href);
        }
    }

    fn navigate_to(&mut self, path: &str) {
        self.add_to_history();
        
        self.page_loading = true;
        self.page_load_start_time = Some(Instant::now());
        let request_path = if path.starts_with('/') { 
            path.to_string() 
        } else { 
            format!("/{}", path) 
        };
        
        if let Err(e) = self.send_request(&request_path) {
            self.error = Some(e);
            self.page_loading = false;
            self.page_load_start_time = None;
        } else {
            self.address_bar = path.to_string();
        }
    }

    // Add current page to history
    fn add_to_history(&mut self) {
        if self.current_history_index < self.history.len().saturating_sub(1) {
            self.history.truncate(self.current_history_index + 1);
        }
        
        let history_entry = HistoryEntry {
            server: self.server_address.clone(),
            page: self.address_bar.clone(),
            content: self.current_content.clone(),
            timestamp: Instant::now(),
        };
        
        self.history.push(history_entry);
        self.current_history_index = self.history.len().saturating_sub(1);
    }

    fn handle_server_message(&mut self, content: String) {
        if content.starts_with("OK\n") {
            self.current_content = content[3..].to_string();
        } else {
            self.current_content = content;
        }
        self.error = None;
        self.page_loading = false;
        self.page_load_start_time = None;
    }

    fn go_back(&mut self) {
        if self.current_history_index > 0 {
            self.current_history_index -= 1;
            if let Some(entry) = self.history.get(self.current_history_index) {
                self.server_address = entry.server.clone();
                self.address_bar = entry.page.clone();
                self.current_content = entry.content.clone();
                self.error = None;
                self.page_loading = false;
                self.page_load_start_time = None;
            }
        }
    }

    fn go_forward(&mut self) {
        if self.current_history_index < self.history.len().saturating_sub(1) {
            self.current_history_index += 1;
            if let Some(entry) = self.history.get(self.current_history_index) {
                self.server_address = entry.server.clone();
                self.address_bar = entry.page.clone();
                self.current_content = entry.content.clone();
                self.error = None;
                self.page_loading = false;
                self.page_load_start_time = None;
            }
        }
    }

    fn reload_current_page(&mut self) {
        if !self.server_address.is_empty() {
            self.page_loading = true;
            self.page_load_start_time = Some(Instant::now());
            
            let path = if self.address_bar.is_empty() {
                "/".to_string()
            } else if self.address_bar.starts_with('/') {
                self.address_bar.clone()
            } else {
                format!("/{}", self.address_bar)
            };

            if let Err(e) = self.send_request(&path) {
                self.error = Some(e);
                self.page_loading = false;
                self.page_load_start_time = None;
            }
        }
    }

    fn show_welcome_page(&self, ui: &mut Ui) {
        ui.vertical_centered(|ui| {
            ui.heading("NymView for Nym Mixnet");
            ui.label("Welcome! Enter a nym:// address to begin.");
            ui.separator();
            
            let demo_content = r#"# NymView for Nym Mixnet
            
## Features:
- **Secure** communication via Nym Mixnet
- **Markdown** support
- **Private** navigation
- **History** navigation (◀ ▶ buttons)
- **Auto-reload** (🔄 button)
- **30-second timeout** for unresponsive servers

### Example content:
- `nym://server/` - Homepage
- `nym://server/about` - About us
- `nym://server/help` - Help

*Enter an address to begin*"#;
            
            CommonMarkViewer::new()
                .show(ui, &mut CommonMarkCache::default(), demo_content);
        });
    }
}

// App Trait Implementation for eframe
impl App for NymMixnetBrowser {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            self.show(ui);
        });
    }
}

impl Clone for NymMixnetBrowser {
    fn clone(&self) -> Self {
        Self {
            address_bar: self.address_bar.clone(),
            current_content: self.current_content.clone(),
            loading: self.loading,
            page_loading: self.page_loading,
            error: self.error.clone(),
            connection_status: self.connection_status.clone(),
            server_address: self.server_address.clone(),
            client_address: self.client_address.clone(),
            message_receiver: None,
            message_sender: None,
            history: self.history.clone(),
            connection_attempted: self.connection_attempted,
            md_cache: CommonMarkCache::default(),
            pending_navigation: None,
            current_history_index: self.current_history_index,
            page_load_start_time: None,
        }
    }
}
