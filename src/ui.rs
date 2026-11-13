use crate::{
    App,
    net::{Peer, State},
};
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Paragraph, canvas::{Canvas, Circle, Line as CLine}},
};
use std::{f64::consts::TAU, time::Duration};

// Noite, duas pessoas lado a lado trocando algo no escuro: tinta violeta, coral como único calor.
pub type Rgb = [u8; 3];
const BG: Rgb = [17, 16, 26];
const FG: Rgb = [236, 230, 222];
const MUTED: Rgb = [132, 127, 160];
const DIM: Rgb = [70, 66, 100];
/// Paleta de identidade: mesma luminosidade e saturação em OKLCH (L .74, C .16), só o matiz muda.
/// O índice 0 (coral) é o padrão e o que vai no beacon se ninguém escolher.
pub const COLORS: [(&str, Rgb); 8] = [
    ("coral", [255, 112, 102]),
    ("âmbar", [227, 154, 7]),
    ("lima", [148, 187, 59]),
    ("verde", [35, 201, 135]),
    ("turquesa", [12, 195, 195]),
    ("azul", [84, 179, 253]),
    ("violeta", [178, 150, 255]),
    ("magenta", [240, 126, 194]),
];
/// Ícones do radar: ASCII de 1 a 5 células (emoji tem largura incerta e desalinha o Braille).
/// O índice 0 é "sem ícone": fica só o ponto. Cabe em 5 bits porque viaja junto da cor.
pub const ICONS: [(&str, &str); 11] = [
    ("ponto", ""),
    ("gato", "=^.^="),
    ("robô", "[o_o]"),
    ("laptop", "[___]"),
    ("fantasma", "(o.o)"),
    ("peixe", "><>"),
    ("cobra", "~~@"),
    ("coruja", "{o,o}"),
    ("coração", "<3"),
    ("estrela", "-*-"),
    ("alien", "/o\\"),
];
const OK: Rgb = [130, 214, 170];
const WARN: Rgb = [255, 190, 90];

fn c(a: Rgb) -> Color {
    Color::Rgb(a[0], a[1], a[2])
}
fn lerp(a: Rgb, b: Rgb, t: f64) -> Rgb {
    let m = |i: usize| (a[i] as f64 * (1.0 - t) + b[i] as f64 * t) as u8;
    [m(0), m(1), m(2)]
}
fn mix(a: Rgb, b: Rgb, t: f64) -> Color {
    c(lerp(a, b, t))
}
fn st(a: Rgb) -> Style {
    Style::new().fg(c(a))
}

pub fn human(n: u64) -> String {
    let (mut v, mut i) = (n as f64, 0);
    while v >= 1024.0 && i < 4 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 { format!("{n} B") } else { format!("{v:.1} {}", ["", "KB", "MB", "GB", "TB"][i]) }
}

/// Ângulo estável de um peer. O raio vem da latência medida (perto = anel interno).
fn angle(id: u64) -> f64 {
    (id.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 40) as f64 / (1u64 << 24) as f64 * TAU
}

/// Ease-out exponencial: sobe rápido e assenta.
fn out_expo(x: f64) -> f64 {
    1.0 - (-6.0 * x.clamp(0.0, 1.0)).exp()
}

pub fn draw(f: &mut Frame, app: &App, peers: &[Peer]) {
    let area = f.area();
    f.render_widget(Block::new().style(Style::new().bg(c(BG)).fg(c(FG))), area);
    let [top, main, bottom] = Layout::vertical([Constraint::Length(1), Constraint::Min(5), Constraint::Length(2)]).areas(area);
    let wide = area.width >= 96;
    let [radar_a, list_a] = if wide {
        Layout::horizontal([Constraint::Percentage(60), Constraint::Percentage(40)]).areas(main)
    } else {
        Layout::vertical([Constraint::Min(10), Constraint::Length(8)]).areas(main)
    };
    let pad = |r: Rect| r.inner(Margin { horizontal: 2, vertical: 0 });
    header(f, pad(top), app, peers.len());
    radar(f, radar_a, app, peers);
    transfers(f, list_a, app, wide);
    footer(f, pad(bottom), app, peers);
}

fn header(f: &mut Frame, a: Rect, app: &App, n: usize) {
    let acc = app.acc();
    let l = Line::from(vec![
        Span::styled("◉ pdrop", st(acc).bold()),
        Span::styled(format!("   visível como {}", app.me), st(MUTED)),
    ]);
    f.render_widget(Paragraph::new(l), a);
    let r = match n {
        0 => format!("procurando{:<3}", ".".repeat((app.anim * 2.0) as usize % 4)),
        1 => "1 por perto".to_string(),
        n => format!("{n} por perto"),
    };
    // pontes de clipboard ativas ficam sempre visíveis
    let mut right = vec![];
    if let Some((_, name, _)) = &app.bridge {
        right.push(Span::styled(format!("ponte → {name}   "), st(acc).bold()));
    }
    let from: Vec<String> = app.grants.lock().unwrap().values().cloned().collect();
    if !from.is_empty() {
        right.push(Span::styled(format!("ponte ← {}   ", from.join(", ")), st(acc).bold()));
    }
    right.push(Span::styled(r, st(MUTED)));
    f.render_widget(Paragraph::new(Line::from(right)).alignment(Alignment::Right), a);
}

fn radar(f: &mut Frame, a: Rect, app: &App, peers: &[Peer]) {
    let acc = app.acc();
    let t = app.anim;
    // Braille: 2x4 pontos por célula, então as unidades do canvas são pontos e círculos ficam redondos.
    let (w, h) = (a.width as f64, a.height as f64);
    let r = (w.min(2.0 * h) - 3.0).max(8.0);
    let sweep = -(t * 1.1); // sentido horário, como radar de verdade
    let granted: Vec<std::net::IpAddr> = app.grants.lock().unwrap().keys().copied().collect();
    let busy: Vec<_> = app.xfers.iter().filter(|x| matches!(x.state, State::Waiting | State::Active)).collect();
    let canvas = Canvas::default()
        .x_bounds([-w, w])
        .y_bounds([-2.0 * h, 2.0 * h])
        .paint(|ctx| {
            for k in [1.0, 2.0 / 3.0, 1.0 / 3.0] {
                ctx.draw(&Circle { x: 0.0, y: 0.0, radius: r * k, color: c(DIM) });
            }
            let p = (t * 0.35) % 1.0; // ondulação saindo de você
            ctx.draw(&Circle { x: 0.0, y: 0.0, radius: r * p, color: mix(acc, BG, 0.5 + 0.5 * p) });
            // rastro da varredura: do mais velho ao mais novo, começando longe do centro
            // (perto do centro as linhas se sobrepõem e a célula Braille vira ruído)
            for k in (0..18).rev() {
                let ang = sweep + k as f64 * 0.04;
                let fade = (-(k as f64) * 0.16).exp();
                let (cs, sn) = (ang.cos(), ang.sin());
                ctx.draw(&CLine { x1: 0.15 * r * cs, y1: 0.15 * r * sn, x2: r * cs, y2: r * sn, color: mix(BG, acc, fade) });
            }
            ctx.layer();
            for p in peers {
                let ang = angle(p.id);
                let pc = COLORS[p.color as usize % COLORS.len()].1;
                let rad = if p.rtt.is_some_and(|ms| ms > 15.0) { 0.83 } else { 0.5 };
                let (x, y) = (r * rad * ang.cos(), r * rad * ang.sin());
                let sel = app.sel == Some(p.id);
                // blip: pico quando a varredura passa e decaimento exponencial
                let since = (ang - sweep).rem_euclid(TAU);
                let ping = (-since * 1.4).exp();
                if since < 1.6 {
                    let burst = out_expo(since / 1.6);
                    ctx.draw(&Circle { x, y, radius: 3.0 + 9.0 * burst, color: mix(BG, pc, (1.0 - burst) * 0.8) });
                }
                // entrada: onda saindo do ponto; saída: some aos poucos quando o silêncio passa de 4s
                let age = p.born.elapsed().as_secs_f64();
                let scale = 0.4 + 0.6 * out_expo(age / 0.6);
                if age < 0.9 {
                    let e = out_expo(age / 0.9);
                    ctx.draw(&Circle { x, y, radius: 2.0 + 16.0 * e, color: mix(pc, BG, e) });
                }
                let gone = ((p.seen.elapsed().as_secs_f64() - 4.0) / 2.0).clamp(0.0, 1.0);
                let base = if sel { pc } else { lerp(MUTED, pc, 0.55 + 0.45 * ping) };
                let col = c(lerp(base, DIM, gone));
                for k in 1..=3 {
                    ctx.draw(&Circle { x, y, radius: k as f64 * scale, color: col });
                }
                if sel {
                    ctx.draw(&Circle { x, y, radius: 7.0 + 1.5 * (t * 4.0).sin(), color: c(lerp(acc, DIM, gone)) });
                }
                let out = app.bridge.as_ref().is_some_and(|(id, ..)| *id == p.id) || busy.iter().any(|x| x.peer == p.name && x.up);
                let inn = granted.contains(&p.addr.ip()) || busy.iter().any(|x| x.peer == p.name && !x.up);
                if out || inn {
                    for i in 0..6 {
                        let fr = (t * 0.7 + i as f64 / 6.0) % 1.0;
                        let fr = if inn && !out { 1.0 - fr } else { fr };
                        ctx.draw(&Circle { x: x * fr, y: y * fr, radius: 1.0, color: c(acc) });
                    }
                }
                let icon = ICONS[(p.color / 8) as usize % ICONS.len()].1;
                if !icon.is_empty() {
                    // órbita leve: elipse baixa em volta do ponto, fase própria por peer
                    let ph = t * 1.3 + ang * 3.0;
                    let n = icon.chars().count() as f64;
                    let (ox, oy) = (x + 6.0 * ph.cos(), y + 4.0 * ph.sin() + 5.0);
                    let ic = lerp(if sel { pc } else { lerp(MUTED, pc, 0.7 + 0.3 * ping) }, DIM, gone);
                    ctx.print(ox - n, oy, Span::styled(icon, Style::new().fg(c(ic)).add_modifier(Modifier::BOLD)));
                }
                let lab = lerp(if sel { pc } else { lerp(MUTED, pc, 0.5 + 0.5 * ping) }, DIM, gone);
                let name = Span::styled(p.name.clone(), Style::new().fg(c(lab)).add_modifier(if sel { Modifier::BOLD } else { Modifier::empty() }));
                ctx.print(x - p.name.chars().count() as f64, y - if icon.is_empty() { 9.0 } else { 13.0 }, name);
            }
            for k in 1..=4 {
                ctx.draw(&Circle { x: 0.0, y: 0.0, radius: k as f64, color: c(FG) });
            }
            ctx.print(-4.0, -10.0, Span::styled("você", st(MUTED)));
            let mine = ICONS[app.pick_icon.unwrap_or(app.icon) as usize].1;
            if !mine.is_empty() {
                let ph = t * 1.3;
                ctx.print(-(mine.chars().count() as f64) + 7.0 * ph.cos(), 9.0 + 4.0 * ph.sin(), Span::styled(mine, Style::new().fg(c(acc)).add_modifier(Modifier::BOLD)));
            }
        });
    f.render_widget(canvas, a);
}

fn transfers(f: &mut Frame, a: Rect, app: &App, wide: bool) {
    let acc = app.acc();
    let blk = Block::new()
        .borders(if wide { Borders::LEFT } else { Borders::TOP })
        .border_style(st(DIM))
        .padding(ratatui::widgets::Padding::horizontal(2));
    let inner = blk.inner(a);
    f.render_widget(blk, a);
    let w = inner.width as usize;
    let mut lines = vec![Line::styled("TRANSFERÊNCIAS", st(MUTED).bold()), Line::raw("")];
    if app.xfers.is_empty() {
        lines.push(Line::styled("Nada por aqui ainda.", st(FG)));
        lines.push(Line::styled("Escolha alguém no radar e aperte enter.", st(MUTED)));
    }
    for t in app.xfers.iter().rev() {
        lines.push(Line::from(vec![
            Span::styled(if t.up { "↑ " } else { "↓ " }, st(acc)),
            Span::styled(t.label.clone(), st(FG).bold()),
            Span::styled(format!("  {} {}", if t.up { "para" } else { "de" }, t.peer), st(MUTED)),
        ]));
        lines.push(match &t.state {
            State::Waiting => Line::styled("  aguardando aceitar…", st(MUTED)),
            State::Active => {
                let bw = w.saturating_sub(18).max(8);
                let fr = if t.size == 0 { 1.0 } else { t.done as f64 / t.size as f64 };
                let n = ((fr * bw as f64) as usize).min(bw);
                Line::from(vec![
                    Span::raw("  "),
                    Span::styled("━".repeat(n), st(acc)),
                    Span::styled("─".repeat(bw - n), st(DIM)),
                    Span::styled(format!(" {:>3}%  {}", (fr * 100.0) as u32, human(t.size)), st(MUTED)),
                ])
            }
            State::Done(m) => {
                let pre = if !t.up && t.kind == 'f' { "salvo em " } else { "" };
                let m: String = m.chars().take(w.saturating_sub(12)).collect();
                Line::from(vec![Span::styled("  ✓ ", st(OK)), Span::styled(format!("{pre}{m}"), st(FG))])
            }
            State::Failed(m) => Line::from(vec![Span::styled("  ✕ ", st(WARN)), Span::styled(m.clone(), st(MUTED))]),
        });
        lines.push(Line::raw(""));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

fn footer(f: &mut Frame, a: Rect, app: &App, peers: &[Peer]) {
    let acc = app.acc();
    let [l1, l2] = Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(a);
    let w = a.width as usize;
    let to = peers.iter().find(|p| Some(p.id) == app.sel).map(|p| p.name.as_str());
    let (line, hints): (Line, &[(&str, &str)]) = if let Some((t, _)) = app.pending.front() {
        let more = app.pending.len() - 1;
        let mut s = vec![
            Span::styled(" ◆ ", st(WARN)),
            Span::styled(t.peer.clone(), st(FG).bold()),
            Span::styled(" quer ", st(MUTED)),
        ];
        match t.kind {
            'c' => s.extend([Span::styled("colar no seu clipboard: ", st(MUTED)), Span::styled(t.note.clone(), st(FG).bold())]),
            'b' => s.extend([Span::styled("ligar uma ponte de clipboard", st(FG).bold()), Span::styled(": tudo que copiar vai para o seu", st(MUTED))]),
            _ => s.extend([
                Span::styled("enviar ", st(MUTED)),
                Span::styled(t.label.clone(), st(FG).bold()),
                Span::styled(format!(" ({})", human(t.size)), st(MUTED)),
            ]),
        }
        if more > 0 {
            s.push(Span::styled(format!("  +{more} na fila"), st(MUTED)));
        }
        (Line::from(s).style(Style::new().bg(mix(BG, WARN, 0.14))), &[("y", "aceitar"), ("n", "recusar")])
    } else if let Some(i) = app.pick {
        let mut sp = vec![Span::raw(" ")];
        for (k, (_, rgb)) in COLORS.iter().enumerate() {
            let on = k == i as usize;
            sp.push(Span::styled(if on { "[●]" } else { " ● " }, Style::new().fg(if on { c(*rgb) } else { mix(*rgb, BG, 0.55) })));
        }
        sp.push(Span::styled(format!("  {}", COLORS[i as usize].0), Style::new().fg(c(COLORS[i as usize].1)).bold()));
        (Line::from(sp), &[("←→", "escolher"), ("enter", "confirmar"), ("esc", "cancelar")])
    } else if let Some(i) = app.pick_icon {
        let (nome, glifo) = ICONS[i as usize];
        let n = ICONS.len();
        let at = |d: usize| ICONS[(i as usize + d) % n].1;
        let gl = |s: &'static str| if s.is_empty() { "·" } else { s };
        let sp = vec![
            Span::raw(" "),
            Span::styled(format!("{:>5} ", gl(at(n - 1))), st(DIM)),
            Span::styled(format!("‹ {} ›", gl(glifo)), st(acc).bold()),
            Span::styled(format!(" {:<5}", gl(at(1))), st(DIM)),
            Span::styled(format!("  {nome}"), st(acc).bold()),
        ];
        (Line::from(sp), &[("←→", "escolher"), ("enter", "confirmar"), ("esc", "cancelar")])
    } else if let Some(buf) = &app.rename {
        let head = "seu nome › ";
        let room = w.saturating_sub(head.chars().count() + 1);
        let tail: String = buf.chars().rev().take(room).collect::<Vec<_>>().into_iter().rev().collect();
        (
            Line::from(vec![Span::styled(head, st(acc)), Span::styled(tail, st(FG)), Span::styled("█", st(acc))]),
            &[("enter", "salvar"), ("esc", "cancelar")],
        )
    } else if let Some(buf) = &app.input {
        let head = format!("para {} › ", to.unwrap_or("?"));
        let room = w.saturating_sub(head.chars().count() + 1);
        let tail: String = buf.chars().rev().take(room).collect::<Vec<_>>().into_iter().rev().collect();
        (
            Line::from(vec![Span::styled(head, st(acc)), Span::styled(tail, st(FG)), Span::styled("█", st(acc))]),
            &[("enter", "enviar"), ("esc", "cancelar"), ("", "caminho do arquivo (ou arraste) · ou texto livre")],
        )
    } else if let Some((m, _)) = app.flash.as_ref().filter(|(_, at)| at.elapsed() < Duration::from_secs(6)) {
        (Line::from(vec![Span::styled("⎘ ", st(acc)), Span::styled(m.clone(), st(FG))]), &[("←→", "escolher"), ("enter", "enviar"), ("v", "clipboard"), ("b", "ponte"), ("q", "sair")])
    } else if let Some(n) = to {
        (
            Line::from(vec![Span::styled("selecionado ", st(MUTED)), Span::styled(n, st(acc).bold())]),
            &[("←→", "escolher"), ("enter", "enviar"), ("v", "clipboard"), ("b", "ponte"), ("c", "cor"), ("i", "ícone"), ("r", "nome"), ("q", "sair")],
        )
    } else {
        (Line::styled("Abra o pdrop em outro aparelho da mesma rede.", st(MUTED)), &[("c", "cor"), ("i", "ícone"), ("r", "nome"), ("q", "sair")])
    };
    f.render_widget(Paragraph::new(line), l1);
    let mut hs: Vec<(&str, &str)> = hints.to_vec();
    if let Some(i) = hs.iter().position(|h| h.0 == "q") {
        if !app.grants.lock().unwrap().is_empty() {
            hs.insert(i, ("x", "cortar ponte"));
        }
    }
    let (mut h, mut used) = (vec![], 0);
    for (k, v) in hs {
        let v = if k == "b" && app.bridge.is_some() { "parar ponte" } else { v };
        let cell = format!("{k}{}{v}   ", if k.is_empty() { "" } else { " " });
        used += cell.chars().count();
        if used > w + 3 {
            break; // o que não cabe sai, na ordem de prioridade
        }
        h.push(Span::styled(k, st(FG).bold()));
        h.push(Span::styled(format!("{}{v}   ", if k.is_empty() { "" } else { " " }), st(MUTED)));
    }
    f.render_widget(Paragraph::new(Line::from(h)), l2);
}
