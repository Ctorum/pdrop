mod clip;
mod net;
mod ui;

use net::{Grants, Msg, Peer, Peers, State, Transfer};
use ratatui::crossterm::{
    event::{self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    execute,
};
use std::{
    collections::VecDeque,
    io::{self, Write},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU8, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub struct App {
    pub me: String,
    pub peers: Peers,
    pub sel: Option<u64>,
    pub input: Option<String>,
    pub xfers: Vec<Transfer>,
    pub pending: VecDeque<(Transfer, Sender<bool>)>,
    /// relógio da animação (segundos); anda mais devagar quando há algo importante na tela
    pub anim: f64,
    pub last: Instant,
    pub calm: bool,
    /// cor confirmada (índice em ui::COLORS), e a que está em pré-visualização enquanto se escolhe
    pub color: u8,
    pub pick: Option<u8>,
    /// ícone confirmado (índice em ui::ICONS; 0 = só o ponto) e o em pré-visualização
    pub icon: u8,
    pub pick_icon: Option<u8>,
    /// edição do próprio nome (texto em andamento)
    pub rename: Option<String>,
    pub shared_name: Arc<Mutex<String>>,
    /// ponte de clipboard que eu liguei: (id do peer, nome, flag de parada)
    pub bridge: Option<(u64, String, Arc<AtomicBool>)>,
    /// pontes que outros ligaram para mim e eu permiti
    pub grants: Grants,
    pub flash: Option<(String, Instant)>,
    pub shared_color: Arc<AtomicU8>,
}

impl App {
    /// cor de acento: a que está em pré-visualização ou, na falta, a confirmada
    pub fn acc(&self) -> ui::Rgb {
        ui::COLORS[self.pick.unwrap_or(self.color) as usize].1
    }

    /// cor e ícone viajam juntos no campo de cor do beacon: cor + 8 * ícone.
    /// Builds antigas fazem `% 8` na cor, então ainda mostram a cor certa.
    fn sync(&self) {
        self.shared_color.store(self.color | self.icon << 3, Ordering::Relaxed);
    }

    pub fn flash(&mut self, s: impl Into<String>) {
        self.flash = Some((s.into(), Instant::now()));
    }

    fn stop_bridge(&mut self) {
        if let Some((_, _, stop)) = self.bridge.take() {
            stop.store(true, Ordering::Relaxed);
        }
    }

    fn clip_once(&mut self, peers: &[Peer], tx: &Sender<Msg>) {
        let Some(p) = peers.iter().find(|p| Some(p.id) == self.sel).cloned() else { return };
        if !clip::available() {
            return self.flash(NO_CLIP);
        }
        match clip::get().filter(|s| !s.is_empty()) {
            None => self.flash("clipboard vazio"),
            Some(text) => {
                let (me, tx) = (self.me.clone(), tx.clone());
                std::thread::spawn(move || {
                    let _ = net::push_clip(&p, &me, &text, 'c', &tx);
                });
            }
        }
    }

    fn toggle_bridge(&mut self, peers: &[Peer], tx: &Sender<Msg>) {
        if self.bridge.is_some() {
            self.stop_bridge();
            return self.flash("ponte desligada");
        }
        let Some(p) = peers.iter().find(|p| Some(p.id) == self.sel).cloned() else { return };
        if !clip::available() {
            return self.flash(NO_CLIP);
        }
        let stop = Arc::new(AtomicBool::new(false));
        net::bridge(p.clone(), self.me.clone(), tx.clone(), stop.clone());
        self.flash(format!("ponte ligada: tudo que você copiar vai para {}. Senhas costumam ficar de fora, mas confira.", p.name));
        self.bridge = Some((p.id, p.name, stop));
    }

    fn answer(&mut self, yes: bool) {
        if let Some((_, r)) = self.pending.pop_front() {
            r.send(yes).ok();
        }
    }

    fn live(&self) -> Vec<Peer> {
        let mut g = self.peers.lock().unwrap();
        g.retain(|_, p| p.seen.elapsed() < Duration::from_secs(6));
        let mut v: Vec<Peer> = g.values().cloned().collect();
        v.sort_by(|a, b| (&a.name, a.id).cmp(&(&b.name, b.id)));
        v
    }

    fn on(&mut self, m: Msg) {
        match m {
            Msg::BridgeOff => {
                self.stop_bridge();
                self.flash("ponte encerrada");
            }
            Msg::Ask(t, r) => {
                notify(&t);
                self.pending.push_back((t, r));
            }
            Msg::Start(t) => {
                self.xfers.push(t);
                if self.xfers.len() > 40 {
                    self.xfers.remove(0);
                }
            }
            Msg::Progress(id, d) => {
                if let Some(t) = self.xfers.iter_mut().find(|t| t.id == id) {
                    (t.done, t.state) = (d, State::Active);
                }
            }
            Msg::End(id, r) => {
                if let Some(t) = self.xfers.iter_mut().find(|t| t.id == id) {
                    t.state = match r {
                        Ok(m) => (t.done = t.size, State::Done(m)).1,
                        Err(e) => State::Failed(e),
                    };
                }
            }
        }
    }

    /// true = sair
    fn key(&mut self, k: KeyEvent, peers: &[Peer], tx: &Sender<Msg>) -> bool {
        if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c') {
            return true;
        }
        if let Some(i) = self.pick_icon {
            let n = ui::ICONS.len() as isize;
            match k.code {
                KeyCode::Left | KeyCode::Char('h') => self.pick_icon = Some((i as isize - 1).rem_euclid(n) as u8),
                KeyCode::Right | KeyCode::Char('l') | KeyCode::Tab => self.pick_icon = Some((i as isize + 1).rem_euclid(n) as u8),
                KeyCode::Enter => {
                    (self.icon, self.pick_icon) = (i, None);
                    self.sync();
                    save_setting("icone", &i.to_string());
                }
                KeyCode::Esc => self.pick_icon = None,
                KeyCode::Char(c @ ('y' | 'n')) => self.answer(c == 'y'),
                _ => {}
            }
            return false;
        }
        if let Some(i) = self.pick {
            let n = ui::COLORS.len() as isize;
            match k.code {
                KeyCode::Left | KeyCode::Char('h') => self.pick = Some((i as isize - 1).rem_euclid(n) as u8),
                KeyCode::Right | KeyCode::Char('l') | KeyCode::Tab => self.pick = Some((i as isize + 1).rem_euclid(n) as u8),
                KeyCode::Enter => {
                    (self.color, self.pick) = (i, None);
                    self.sync();
                    save_setting("cor", &i.to_string());
                }
                KeyCode::Esc => self.pick = None,
                KeyCode::Char(c @ ('y' | 'n')) => self.answer(c == 'y'),
                _ => {}
            }
            return false;
        }
        if let Some(buf) = &mut self.rename {
            match k.code {
                KeyCode::Esc => self.rename = None,
                KeyCode::Backspace => {
                    buf.pop();
                }
                KeyCode::Char(c) if buf.chars().count() < NAME_MAX => buf.push(c),
                KeyCode::Enter => {
                    let n = net::clean(buf.trim());
                    self.rename = None;
                    if !n.is_empty() {
                        *self.shared_name.lock().unwrap() = n.clone();
                        save_setting("nome", &n);
                        self.me = n;
                    }
                }
                _ => {}
            }
            return false;
        }
        if let Some(buf) = &mut self.input {
            match k.code {
                KeyCode::Esc => self.input = None,
                KeyCode::Backspace => {
                    buf.pop();
                }
                KeyCode::Char(c) => buf.push(c),
                KeyCode::Enter => {
                    let text = self.input.take().unwrap_or_default();
                    let peer = peers.iter().find(|p| Some(p.id) == self.sel);
                    if let (Some(p), false) = (peer, text.trim().is_empty()) {
                        net::send(p.clone(), self.me.clone(), text, tx.clone());
                    }
                }
                _ => {}
            }
            return false;
        }
        let step = |d: isize| {
            let i = peers.iter().position(|p| Some(p.id) == self.sel).unwrap_or(0) as isize;
            (!peers.is_empty()).then(|| peers[(i + d).rem_euclid(peers.len() as isize) as usize].id)
        };
        match k.code {
            KeyCode::Char('q') | KeyCode::Esc => return true,
            KeyCode::Left | KeyCode::Char('h') | KeyCode::BackTab => self.sel = step(-1).or(self.sel),
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Tab => self.sel = step(1).or(self.sel),
            KeyCode::Enter | KeyCode::Char('s') if self.sel.is_some() => self.input = Some(String::new()),
            KeyCode::Char('c') => self.pick = Some(self.color),
            KeyCode::Char('i') => self.pick_icon = Some(self.icon),
            KeyCode::Char('v') => self.clip_once(peers, tx),
            KeyCode::Char('b') => self.toggle_bridge(peers, tx),
            KeyCode::Char('x') => {
                self.grants.lock().unwrap().clear();
                self.flash("pontes recebidas cortadas");
            }
            KeyCode::Char('r') => self.rename = Some(self.me.clone()),
            KeyCode::Char(c @ ('y' | 'n')) => self.answer(c == 'y'),
            _ => {}
        }
        false
    }
}

/// Avisa fora do terminal (o pdrop pode estar em segundo plano): bell + notificação do sistema.
/// Remetente e nome do arquivo vêm da rede, então vão como argumentos, nunca montados em comando.
fn notify(t: &Transfer) {
    let body = format!("{} quer enviar {} ({})", t.peer, t.label, ui::human(t.size));
    let mut out = io::stdout();
    let _ = out.write_all(b"\x07").and_then(|_| out.flush());
    std::thread::spawn(move || {
        #[cfg(target_os = "macos")]
        let _ = std::process::Command::new("osascript")
            .args(["-e", "on run {t, b}", "-e", "display notification b with title t sound name \"Ping\"", "-e", "end run", "pdrop", &body])
            .status();
        #[cfg(not(target_os = "macos"))]
        let _ = std::process::Command::new("notify-send")
            .args(["-a", "pdrop", "-u", "critical", "pdrop"])
            .arg(format!("◆ {}", body.replace('&', "&amp;").replace('<', "&lt;")))
            .status();
    });
}

const NAME_MAX: usize = 32;
const NO_CLIP: &str = "sem clipboard: instale wl-clipboard (Wayland) ou xclip (X11)";

fn setting_path(k: &str) -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into())).join(".config/pdrop").join(k)
}

fn load_setting(k: &str) -> Option<String> {
    std::fs::read_to_string(setting_path(k)).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// Falhar ao gravar não pode derrubar o app: o valor vale na sessão e só não persiste.
fn save_setting(k: &str, v: &str) {
    let p = setting_path(k);
    let _ = std::fs::create_dir_all(p.parent().unwrap()).and_then(|_| std::fs::write(p, v));
}

fn load_color() -> u8 {
    let c = load_setting("cor").and_then(|s| s.parse::<u8>().ok());
    c.filter(|&c| (c as usize) < ui::COLORS.len()).unwrap_or(0)
}

fn load_icon() -> u8 {
    let i = load_setting("icone").and_then(|s| s.parse::<u8>().ok());
    i.filter(|&i| (i as usize) < ui::ICONS.len()).unwrap_or(0)
}

fn hostname() -> String {
    let h = std::fs::read_to_string("/etc/hostname")
        .ok()
        .or_else(|| std::process::Command::new("hostname").output().ok().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()))
        .unwrap_or_default();
    let h = net::clean(h.trim().split('.').next().unwrap_or(""));
    if h.is_empty() { "pdrop".into() } else { h }
}

fn main() -> io::Result<()> {
    let mut args = std::env::args().skip(1);
    let mut me = load_setting("nome").map(|n| net::clean(&n)).unwrap_or_else(hostname); // --name vale só na sessão
    let mut direct: Vec<std::net::IpAddr> = vec![];
    let mut calm = false;
    while let Some(a) = args.next() {
        if a == "--calm" {
            calm = true;
            continue;
        }
        match (a.as_str(), args.next()) {
            ("--name" | "-n", Some(n)) => me = net::clean(&n),
            ("--peer" | "-p", Some(ip)) if ip.parse::<std::net::IpAddr>().is_ok() => direct.push(ip.parse().unwrap()),
            _ => {
                eprintln!("uso: pdrop [--name NOME] [--peer IP]... [--calm]\nrecebidos vão para ~/Downloads");
                return Ok(());
            }
        }
    }
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos() as u64;
    let id = nanos ^ ((std::process::id() as u64) << 32);
    let dir = PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into())).join("Downloads");
    let (tx, rx) = mpsc::channel();
    let peers = Peers::default();
    let grants = Grants::default();
    let port = net::listen(dir, tx.clone(), grants.clone())?;
    let color = load_color();
    let icon = load_icon();
    let shared_color = Arc::new(AtomicU8::new(color | icon << 3));
    let shared_name = Arc::new(Mutex::new(me.clone()));
    net::discover(id, shared_name.clone(), port, shared_color.clone(), peers.clone(), direct)?;

    let app = App { me, peers, sel: None, input: None, xfers: vec![], pending: VecDeque::new(), anim: 0.0, last: Instant::now(), calm, color, pick: None, icon, pick_icon: None, rename: None, shared_name, shared_color, bridge: None, grants, flash: None };
    let mut term = ratatui::init();
    execute!(io::stdout(), EnableBracketedPaste)?;
    let res = run(&mut term, app, rx, tx);
    execute!(io::stdout(), DisableBracketedPaste).ok();
    ratatui::restore();
    res
}

fn run(term: &mut ratatui::DefaultTerminal, mut app: App, rx: Receiver<Msg>, tx: Sender<Msg>) -> io::Result<()> {
    loop {
        while let Ok(m) = rx.try_recv() {
            app.on(m);
        }
        let now = Instant::now();
        let busy = app.input.is_some() || app.rename.is_some() || !app.pending.is_empty();
        let speed = if busy { 0.3 } else if app.calm { 0.5 } else { 1.0 };
        app.anim += now.duration_since(app.last).as_secs_f64() * speed;
        app.last = now;
        let peers = app.live();
        if app.bridge.as_ref().is_some_and(|(id, ..)| !peers.iter().any(|p| p.id == *id)) {
            app.stop_bridge();
            app.flash("ponte encerrada: o aparelho saiu da rede");
        }
        if !peers.iter().any(|p| Some(p.id) == app.sel) {
            app.sel = peers.first().map(|p| p.id);
        }
        term.draw(|f| ui::draw(f, &app, &peers))?;
        if event::poll(Duration::from_millis(40))? {
            match event::read()? {
                Event::Key(k) if k.kind == KeyEventKind::Press => {
                    if app.key(k, &peers, &tx) {
                        return Ok(());
                    }
                }
                // arrastar um arquivo para o terminal cola o caminho: abre o envio já preenchido
                Event::Paste(s) => {
                    let s = s.trim().replace('\n', " ");
                    match &mut app.input {
                        Some(b) => b.push_str(&s),
                        None if app.sel.is_some() => app.input = Some(s),
                        None => {}
                    }
                }
                _ => {}
            }
        }
    }
}
