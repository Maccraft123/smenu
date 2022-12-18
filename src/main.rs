use sgui::layout::Layout;
use sgui::Gui;
use sgui::GuiEvent;

use nix::{
    ioctl_write_int_bad,
    sys::signal::Signal,
};
use serde::{Serialize, Deserialize};
use anyhow::{anyhow, Result, Context};
use std::{
    env,
    process::{Command, Stdio},
    fs::{self, File, OpenOptions},
    thread,
    io::{
        Read, BufReader, BufRead,
        Write,
    },
    path::{Path, PathBuf},
    os::unix::{
        io::AsRawFd,
        process::ExitStatusExt,
    },
    collections::{HashSet, HashMap},
};

use libdogd::{log_debug, log_info, log_error, log_critical, LogPriority, post_log, log_rust_error};

#[derive(Debug, Serialize, Deserialize)]
enum Category {
    Tools,
    Programs,
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
struct Emulator {
    executable: PathBuf,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: Vec<(String, String)>,
    systems: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct System {
    name: String,
    rom_directory: PathBuf,
    file_extensions: HashSet<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MenuLayout {
    #[serde(rename = "item")]
    items: Vec<MenuEntry>,
    #[serde(rename = "emulator")]
    emulators: Vec<Emulator>,
    #[serde(rename = "system")]
    systems: Vec<System>,
}

impl MenuLayout {
    fn mk_sgui_layout(self) -> (HashMap<u128, MenuEntry>, Layout) {
        let mut id = 0;
        let mut entry_map = HashMap::new();
        let mut tools = Vec::new();
        let mut programs = Vec::new();
        let mut roms = Vec::new();

        for item in self.items.into_iter() {
            match item.category {
                Category::Tools => tools.push((item, id)),
                Category::Programs => programs.push((item, id)),
            }
            id += 1;
        }

        for system in self.systems.iter() {
            if system.rom_directory.exists() {
                let files = match fs::read_dir(&system.rom_directory) {
                    Ok(f) => f,
                    Err(e) => {
                        log_rust_error(&e, format!("Failed to open rom directory for {}", &system.name), LogPriority::Error);
                        continue;
                    }
                };
                let Some(emulator) = self.emulators.iter().find(|e| e.systems.contains(&system.name)) else {
                    log_error(format!("Failed to find suitable emulator for system {}", &system.name));
                    continue;
                };

                let mut system_tab = Vec::new();
                for file in files {
                    let Ok(file) = file else { continue };
                    let Ok(filename) = file.file_name().into_string() else { continue };
                    let ext = filename.split('.').last().unwrap_or("<invalid>");
                    if !system.file_extensions.contains(&ext.to_string()) {
                        log_error(format!("Wrong file extension for file {}. Expected one of: {:?}, Got: {}", filename, &system.file_extensions, ext));
                        continue;
                    }
                    let fancy_name = filename.split('.').next().unwrap().to_string();
                    let mut args = emulator.args.clone();
                    args.push(file.path().into_os_string().into_string().unwrap_or("".to_string()));

                    let entry = MenuEntry {
                        name: fancy_name,
                        category: Category::Tools, // ignored
                        uses_wayland: true,
                        executable: emulator.executable.clone(),
                        args,
                        env: emulator.env.clone(),
                    };
                    system_tab.push((entry, id));
                    id += 1;
                }
                roms.push((system.name.clone(), system_tab));
            } else {
                log_error(format!("{}'s rom directory, {}, does not exist, skipping ", &system.name, system.rom_directory.display()));
            }
        }

        let layout = Layout::builder();
        let mut tools_tab = layout.tab("System Tools");
        for (entry, id) in tools {
            tools_tab = tools_tab.line().button_stateless(&entry.name, id).endl();
            entry_map.insert(id, entry);
        }
        let layout = tools_tab.end_tab();

        let mut programs_tab = layout.tab("Programs");
        for (entry, id) in programs {
            programs_tab = programs_tab.line().button_stateless(&entry.name, id).endl();
            entry_map.insert(id, entry);
        }
        let mut layout = programs_tab.end_tab();

        for (name, romtab) in roms {
            let mut tab = layout.tab(&name);
            for (entry, id) in romtab {
                tab = tab.line().button_stateless(&entry.name, id).endl();
                entry_map.insert(id, entry);
            }
            layout = tab.end_tab();
        }
        
        (entry_map, layout.build())
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
        if env::var("XDG_RUNTIME_DIR").is_err() {
            envs.push(("XDG_RUNTIME_DIR".to_string(), "/xdg".to_string()));
        } else {
            log_info("Detected XDG_RUNTIME_DIR env var present, /not/ setting it");
        }
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

fn save_config(l: &MenuLayout, p: &Path) -> Result<()> {
    let conf = toml::to_string(l).context("Failed to serialize config")?;
    let mut conf_file = File::create(p).context("Failed to create/open config file")?;
    conf_file.write_all(conf.as_bytes()).context("Failed to save config")?;
    Ok(())
}

fn load_config(p: &Path) -> Result<MenuLayout> {
    if !p.exists() {
        return Err(anyhow!("Missing config file, using default"));
    }
    let conf = fs::read_to_string(p).context("Failed to read config file")?;
    let config = toml::from_str(&conf).context("Failed to deserialize config file")?;
    Ok(config)
}

fn load_default_config() -> Result<MenuLayout> {
    let conf = include_str!("default.toml");
    Ok(toml::from_str(conf)?)
}

fn main() {
    let config_path = PathBuf::from("/etc/smenu.toml");
    log_debug(format!("Loading config from {}", config_path.display()));
    //let menu_layout = load_config(&config_path).unwrap_or(load_default_config().unwrap());
    let menu_layout = match load_config(&config_path) {
        Ok(c) => c,
        Err(e) => {
            log_rust_error(&*e, "Failed to load config", LogPriority::Error);
            load_default_config().unwrap()
        },
    };

    let (entries, layout) = menu_layout.mk_sgui_layout();
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
                if let Some(entry) = entries.get(&id) {
                    gui.set_ignore_hid(true);
                    thread::scope(|s| {
                        let h = s.spawn(move || {if let Err(e) = run_entry(&entry) {
                            log_rust_error(&*e, "Failed to run menu entry", LogPriority::Error);
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

    log_debug(format!("Saving config into {}", config_path.display()));
    //if let Err(e) = save_config(&menu_layout, &config_path) {
    //    log_rust_error(&*e, "Failed to save menu config", LogPriority::Critical);
    //}
}
