use sgui::layout::Layout;
use sgui::Gui;
use sgui::GuiEvent;

use nix::{
    ioctl_write_int_bad,
    sys::signal::Signal,
};
use serde::{Serialize, Deserialize};
use anyhow::{Result, Context};
use std::{
    process::{Command, Stdio},
    path::PathBuf,
    fs::{File, OpenOptions},
    thread,
    io::{
        Read, BufReader, BufRead,
        Write,
    },
    os::unix::{
        io::AsRawFd,
        process::ExitStatusExt,
    },
};

use libdogd::{log_debug, log_info, log_error, log_critical, LogPriority, post_log};

#[derive(Debug, Serialize, Deserialize)]
enum Category {
    Tools,
    Programs,
    Emulators,
}

#[derive(Debug, Serialize, Deserialize)]
struct MenuEntry {
    name: String,
    category: Category,
    uses_wayland: bool,
    executable: PathBuf,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: Vec<(String, String)>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MenuLayout {
    #[serde(rename = "item")]
    items: Vec<MenuEntry>,
}

impl MenuLayout {
    fn mk_sgui_layout(&self) -> Layout {
        let mut tools = Vec::new();
        let mut programs = Vec::new();

        for (id, item) in self.items.iter().enumerate() {
            match item.category {
                Category::Tools => tools.push((&item.name, id )),
                Category::Programs => programs.push((&item.name, id)),
                Category::Emulators => todo!("emulators are not supported yet"),
                //Category::Emulators => emulators.insert((&item.name, id))
            }
        }

        let layout = Layout::builder();
        let mut tools_tab = layout.tab("System Tools");
        for (name, id) in tools {
            tools_tab = tools_tab.line().button_stateless(name, id as u128).endl();
        }

        let mut programs_tab = tools_tab.tab("Programs");
        for (name, id) in programs {
            programs_tab = programs_tab.line().button_stateless(name, id as u128).endl();
        }
        
        programs_tab.build()
    }
}

ioctl_write_int_bad!(vt_activate, 0x5606);
ioctl_write_int_bad!(vt_waitactive, 0x5607);
fn switch_tty(num: i32, clear: bool) -> Result<()> {
    if unsafe{ libc::geteuid() } != 0 {
        log_info("Running as a non-root user, ignoring TTY changes");
        return Ok(());
    }

    let file = OpenOptions::new().read(true).write(true).open("/dev/tty")
        .or_else(|_| OpenOptions::new().read(true).write(true).open("/dev/tty0"))?;
    unsafe { vt_activate(file.as_raw_fd(), num) }?;
    unsafe { vt_waitactive(file.as_raw_fd(), num) }?;
    if clear {
        let mut tty = OpenOptions::new().read(false).write(true).open(format!("/dev/tty{}", num))?;
        tty.write_all(b"\x1B[2J\x1B[1;1H")?;
    }
    Ok(())
}

fn push2dogd(stream: impl Read, name: String, priority: LogPriority) {
    let mut writer = BufReader::new(stream);
    let mut buf = String::new();

    while let Ok(_) = writer.read_line(&mut buf) {
        if buf.is_empty() {
            continue;
        }
        post_log(&buf, &name, priority);
        buf.clear();
    }
}

fn run_entry(e: &MenuEntry) -> Result<()> {
    log_debug(format!("Running {}", &e.name));
    let mut envs = e.env.clone();
    let stdin;
    let stdout;
    let stderr;
    if e.uses_wayland {
        switch_tty(2, false).context("Failed switch to tty2")?;
        stdin = Stdio::null();
        stdout = Stdio::piped();
        stderr = Stdio::piped();
    } else {
        switch_tty(3, true).context("Failed to switch to tty3")?;
        stdin = File::open("/dev/tty3").context("Failed to open tty3 for reading")?.into();
        stdout = File::create("/dev/tty3").context("Failed to open tty3 for writing")?.into();
        stderr = File::create("/dev/tty3").context("Failed to open tty3 for writing")?.into();
        envs.push(("TERM".to_string(), "linux".to_string()));
    }

    let mut child = Command::new(&e.executable)
        .args(&e.args)
        .envs(e.env.clone())
        .stdin(stdin)
        .stdout(stdout)
        .stderr(stderr)
        .spawn()
        .context("Failed to spawn the program")?;

    let name = e.executable.into_iter().last().unwrap().to_string_lossy().to_string();

    if let Some(child_stdout) = child.stdout.take() {
        let name = name.clone();
        thread::spawn(move || push2dogd(child_stdout, name, LogPriority::Info));
    } else {
        log_error("Failed to get stdout handle, logs are incomplete");
    }

    if let Some(child_stderr) = child.stderr.take() {
        let name = name.clone();
        thread::spawn(move || push2dogd(child_stderr, name, LogPriority::Error));
    } else {
        log_error("Failed to get stderr handle, logs are incomplete");
    }
    
    let result = child.wait().context("Failed to wait for program to exit")?;
    if let Some(code) = result.code() {
        if code != 0 {
            log_critical(format!("Application {} returned with erroneous code {}!\nCheck logs on data partition", name, code));
        }
    }

    if let Some(sig) = result.signal() {
        log_critical(format!("Application {} returned due to {:?}!\nCheck logs on data partition", name, Signal::try_from(sig)));
    }

    switch_tty(1, false).context("Failed to switch back to tty1")?;
    Ok(())
}

fn save_config(l: &MenuLayout) {
    let conf = toml::to_string(l).expect("Failed to serialize config");
    eprintln!("{}", conf);
}

fn main() {
    let menu_layout = MenuLayout {
        items: vec![
            MenuEntry {
                name: "Toggle SSH".to_string(),
                category: Category::Tools,
                uses_wayland: false,
                executable: PathBuf::from("/smenu/toggle_ssh"),
                args: vec![],
                env: vec![],
            },
            MenuEntry {
                name: "Power Off".to_string(),
                category: Category::Tools,
                uses_wayland: false,
                executable: PathBuf::from("/sbin/poweroff"),
                args: vec![],
                env: vec![],
            },
            MenuEntry {
                name: "Htop".to_string(),
                category: Category::Tools,
                uses_wayland: false,
                executable: PathBuf::from("/usr/bin/htop"),
                args: vec![],
                env: vec![],
            },
            MenuEntry {
                name: "Open shell".to_string(),
                category: Category::Tools,
                uses_wayland: false,
                executable: PathBuf::from("/usr/bin/ash"),
                args: vec![],
                env: vec![],
            },
            MenuEntry {
                name: "Launch kmscube".to_string(),
                category: Category::Tools,
                uses_wayland: false,
                executable: PathBuf::from("/usr/bin/kmscube"),
                args: vec![],
                env: vec![],
            },
        ],
    };

    let layout = menu_layout.mk_sgui_layout();
    log_debug("Smenu starting up");
    let mut gui = Gui::new(layout);
    let state = loop {
        let ev = gui.get_ev();
        match ev {
            GuiEvent::Quit => {
                let state = gui.exit_dumping_state();
                break state;
            },
            GuiEvent::StatelessButtonPress(_, id) => {
                if let Some(entry) = menu_layout.items.get(id as usize) {
                    gui.set_ignore_hid(true);
                    thread::scope(|s| {
                        let h = s.spawn(move || {if let Err(e) = run_entry(&entry) {
                            let msg = e.chain()
                                .map(|e| e.to_string().to_string())
                                .map(|v| v + "\n")
                                .collect::<String>();
                            log_critical(format!("Failed to run entry, due to:\n{}", msg));
                        }});
                        while !h.is_finished() {
                            let _ = gui.get_ev();
                        }

                    });
                    gui.set_ignore_hid(false);
                }
            },
            _ => (),
        }
    };

    save_config(&menu_layout);
    println!("{:#?}", state);
}
