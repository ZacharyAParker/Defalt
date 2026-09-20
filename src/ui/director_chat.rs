//! A modeless, private conversation alongside the running radio.
use egui::{Color32, RichText};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use super::theme;

pub struct Chat {
    pub preview: bool,
    pub open: bool,
    pub draft: String,
    pub state: serde_json::Value,
    save: bool,
    share: bool,
    port: u16,
    inflight: bool,
    last_poll: Instant,
    error: Option<String>,
    retry: Option<serde_json::Value>,
    incoming: Receiver<(bool, Result<serde_json::Value,String>)>,
    outgoing: Sender<(bool, Result<serde_json::Value,String>)>,
}

impl Chat {
    pub fn new(port: u16) -> Self {
        let (outgoing,incoming)=channel();
        Self {preview:false,open:false,draft:String::new(),state:serde_json::json!({}),save:false,share:false,
            port,inflight:false,last_poll:Instant::now()-Duration::from_secs(5),error:None,retry:None,incoming,outgoing}
    }

    fn request(&mut self, body: Option<serde_json::Value>) {
        self.inflight=true;
        self.last_poll=Instant::now();
        let url=format!("http://127.0.0.1:{}/api/director/chat",self.port);
        let sender=self.outgoing.clone();
        std::thread::spawn(move || {
            let sent=body.is_some();
            let result=(|| {
                let mut response=if let Some(body)=body {
                    ureq::post(&url).config().http_status_as_error(false).timeout_global(Some(Duration::from_secs(8))).build().send_json(body)
                } else {
                    ureq::get(&url).config().http_status_as_error(false).timeout_global(Some(Duration::from_secs(8))).build().call()
                }.map_err(|_| "Can't reach the director. Start Radio, then retry.".to_string())?;
                let status=response.status().as_u16();
                let value:serde_json::Value=response.body_mut().read_json().map_err(|_| "The director returned an unreadable reply. Retry your message.".to_string())?;
                if status>=400 {return Err(value["error"].as_str().unwrap_or("Director chat is unavailable. Start Radio or update its backend.").to_string());}
                Ok(value)
            })();
            let _=sender.send((sent,result));
        });
    }

    fn send(&mut self) {
        let message=self.draft.trim();
        if message.is_empty() || self.inflight || self.state["busy"].as_bool()==Some(true) {return;}
        let mut body=serde_json::json!({"message":message,"save":self.save,"share":self.share});
        if let Some(previous)=&self.retry {
            if previous["message"]==body["message"] && previous["save"]==body["save"] && previous["share"]==body["share"] { body["id"]=previous["id"].clone(); }
        }
        if body["id"].is_null() {
            body["id"]=format!("desktop-{}-{}",std::process::id(),SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos()).into();
        }
        self.retry=Some(body.clone());
        self.error=None;
        self.request(Some(body));
    }

    pub fn draw(&mut self, ctx:&egui::Context, running:bool) {
        while let Ok((sent,result))=self.incoming.try_recv() {
            self.inflight=false;
            match result {
                Ok(state)=> {self.state=state;self.error=None;if sent {self.draft.clear();self.share=false;self.save=false;self.retry=None;}},
                Err(error)=>self.error=Some(error),
            }
        }
        if !self.open {return;}
        if running && !self.preview && !self.inflight && self.last_poll.elapsed()>Duration::from_secs(2) {self.request(None);}
        let mut open=self.open;
        egui::Window::new("Director chat").open(&mut open).default_width(540.).default_height(510.)
            .default_pos(egui::pos2(260.,80.))
            .frame(egui::Frame::window(&ctx.style_of(egui::Theme::Dark)).fill(theme::PANEL))
            .min_width(330.).max_height((ctx.content_rect().height()-100.).max(300.))
            .show(ctx,|ui| {
                ui.label(RichText::new("Steer the station, keep the music going.").size(17.).color(theme::TEXT));
                ui.label(RichText::new("Private unless you choose to send a message to the hosts.").color(theme::TEXT_DIM));
                if let Some(direction)=self.state["direction"]["description"].as_str() {
                    ui.add_space(6.);ui.label(RichText::new(format!("Music direction: {direction}")).color(theme::CYAN));
                }
                if let Some(minutes)=self.state["quiet_minutes"].as_f64().filter(|v|*v>0.) {ui.label(format!("Fewer host breaks: {minutes:.0} min left"));}
                ui.separator();
                egui::ScrollArea::vertical().id_salt("private-director-conversation").stick_to_bottom(true)
                    .max_height((ctx.content_rect().height()*0.37).clamp(150.,350.)).min_scrolled_height(150.)
                    .show(ui,|ui| {
                        let messages=self.state["messages"].as_array();
                        if messages.is_none_or(|m|m.is_empty()) {
                            ui.label(RichText::new("Tell me what you're in the mood for.").size(18.));
                            ui.add_space(8.);
                            for example in ["Keep this energy, but less rap.","That last pick was perfect. More like that.","Less talking for twenty minutes.","Why did you choose this song?"] {
                                if ui.button(example).clicked() {self.draft=example.into();}
                            }
                        }
                        for message in messages.into_iter().flatten() {
                            let yours=message["role"]=="user";
                            ui.label(RichText::new(if yours {"You"} else {"Director"}).strong().color(if yours {theme::TEXT_DIM}else{theme::CYAN}));
                            ui.label(message["text"].as_str().unwrap_or(""));
                            if message["shared"]==true {ui.label(RichText::new("Sent with permission to share on air").small().color(theme::TEXT_DIM));}
                            ui.add_space(12.);
                        }
                    });
                let busy=self.state["busy"].as_bool().unwrap_or(false);
                if busy {ui.horizontal(|ui| {ui.spinner();ui.label("Director is replying...");});}
                if let Some(error)=&self.error {ui.colored_label(Color32::from_rgb(245,180,130),error);}
                if !running {ui.label("Start Radio to chat with the director.");}
                ui.separator();
                let entry=ui.add(egui::TextEdit::multiline(&mut self.draft).desired_rows(3).desired_width(f32::INFINITY)
                    .char_limit(2000).hint_text("What should we play next?"));
                ui.horizontal_wrapped(|ui| {
                    ui.checkbox(&mut self.save,"Save music direction").on_hover_text("Keep a music direction across restarts. Ordinary messages apply to this session only.");
                    ui.checkbox(&mut self.share,"Send this to hosts").on_hover_text("Share this message on air instead of changing station controls. Maximum 240 characters.");
                });
                let allowed=running && !busy && !self.inflight && !self.draft.trim().is_empty() && (!self.share || self.draft.chars().count()<=240);
                let enter=entry.has_focus() && ui.input(|i|i.modifiers.ctrl && i.key_pressed(egui::Key::Enter));
                ui.horizontal(|ui| {
                    if ui.add_enabled(allowed,egui::Button::new(if self.share {"Send to hosts"} else {"Send"})).clicked() || (allowed && enter) {self.send();}
                    if ui.add_enabled(allowed || (running && !busy && !self.inflight),egui::Button::new("Undo last change")).clicked() {self.draft="Undo last change".into();self.share=false;self.save=false;self.send();}
                    ui.label(RichText::new("Ctrl+Enter to send").small().color(theme::TEXT_DIM));
                });
                ui.label(RichText::new("Prepared mixes finish first. Changes start with unprepared picks.").small().color(theme::TEXT_DIM));
            });
        self.open=open;
        ctx.request_repaint_after(Duration::from_millis(if self.inflight {100}else{1000}));
    }
}
