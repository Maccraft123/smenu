use sgui::layout::Layout;
use sgui::Gui;
use sgui::GuiEvent;

use nix::ioctl_write_int_bad;
use serde::{Serialize, Deserialize};
use anyhow::Result;
use std::{
    process::{Command, Stdio},
    path::PathBuf,
    fs::OpenOptions,
    os::unix::io::AsRawFd,
};

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

        let mut layout = Layout::builder();
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
fn switch_tty(num: i32) -> Result<()> {
    let file = OpenOptions::new().read(true).write(true).open("/dev/console")
        .or_else(|_| OpenOptions::new().read(true).write(true).open("/dev/tty"))
        .or_else(|_| OpenOptions::new().read(true).write(true).open("/dev/tty0"))?;
    unsafe { vt_activate(file.as_raw_fd(), num) }?;
    unsafe { vt_waitactive(file.as_raw_fd(), num) }?;
    Ok(())
}

fn run_entry(e: &MenuEntry) -> Result<()> {
    eprintln!("Running {}", &e.name);
    if e.uses_wayland {
        switch_tty(2)?;
    } else {
        switch_tty(3)?;
    }

    let result = Command::new(&e.executable)
        .args(&e.args)
        .envs(e.env.clone())
        .stdin(Stdio::null())
        .output()?;

    if !result.status.success() {
        todo!("Handling failures");
    }

    switch_tty(1)?;
    Ok(())
}

fn save_config(l: MenuLayout) {
    let conf = toml::to_string(&l).expect("Failed to serialize config");
    eprintln!("{}", conf);
}

fn main() {
    let ssh = false;
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
                executable: PathBuf::from("/smenu/power_off"),
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
                name: "Open Weston Terminal".to_string(),
                category: Category::Programs,
                uses_wayland: true,
                executable: PathBuf::from("/usr/bin/weston-terminal"),
                args: vec![],
                env: vec![],
            },
        ],
    };

    let layout = menu_layout.mk_sgui_layout();
    eprintln!("{:#?}", &layout);
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
                    run_entry(&entry);
                }
            },
            _ => {
                eprintln!("{:#?}", &ev);
            }
        }
    };

    save_config(menu_layout);
    println!("{:#?}", state);
}
