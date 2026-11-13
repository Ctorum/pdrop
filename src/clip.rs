//! Clipboard de texto via ferramentas do sistema (sem dependência): wl-clipboard, xclip/xsel, pbcopy/pbpaste.
use std::{
    io::{self, Read, Write},
    process::{Command, Stdio},
    sync::{Mutex, OnceLock},
};

type Cmds = (&'static [&'static str], &'static [&'static str]);

fn which(bin: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(bin).is_file()))
}

fn tools() -> Option<Cmds> {
    static T: OnceLock<Option<Cmds>> = OnceLock::new();
    *T.get_or_init(|| {
        let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();
        if which("pbpaste") {
            Some((&["pbpaste"], &["pbcopy"]))
        } else if wayland && which("wl-paste") && which("wl-copy") {
            Some((&["wl-paste", "-n"], &["wl-copy"]))
        } else if which("xclip") {
            Some((&["xclip", "-selection", "clipboard", "-o"], &["xclip", "-selection", "clipboard", "-i"]))
        } else if which("xsel") {
            Some((&["xsel", "-ob"], &["xsel", "-ib"]))
        } else {
            None
        }
    })
}

pub fn available() -> bool {
    tools().is_some()
}

/// Última coisa que o pdrop viu ou escreveu no clipboard: evita reenviar o que acabou de chegar (eco).
static LAST: Mutex<String> = Mutex::new(String::new());

pub fn get() -> Option<String> {
    let (cmd, _) = tools()?;
    let mut child = Command::new(cmd[0]).args(&cmd[1..]).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
    let mut buf = String::new();
    // limita a leitura: não carrega um clipboard gigante na memória
    child.stdout.take()?.take(2 << 20).read_to_string(&mut buf).ok()?;
    child.wait().ok()?.success().then_some(buf)
}

pub fn set(text: &str) -> io::Result<()> {
    let (_, cmd) = tools().ok_or_else(|| io::Error::other("sem ferramenta de clipboard"))?;
    *LAST.lock().unwrap() = text.to_string();
    let mut child = Command::new(cmd[0]).args(&cmd[1..]).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn()?;
    child.stdin.take().unwrap().write_all(text.as_bytes())?;
    child.wait()?;
    Ok(())
}

/// Marca o conteúdo atual como "já visto": a ponte só envia o que for copiado depois de ligada.
pub fn mark_current() {
    *LAST.lock().unwrap() = get().unwrap_or_default();
}

/// true se `cur` é novo (e passa a ser o último visto).
pub fn changed(cur: &str) -> bool {
    let mut l = LAST.lock().unwrap();
    (*l != cur).then(|| *l = cur.to_string()).is_some()
}
