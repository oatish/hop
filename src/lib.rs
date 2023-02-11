pub mod args;
use anyhow;
use args::{Cmd, Rabbit};
use chrono::Local;
use colored::Colorize;
use dirs::home_dir;
use press_btn_continue;
use serde_derive::Deserialize;
use sqlite;
use std::{
    collections::HashMap,
    env::current_exe,
    env::{current_dir, var},
    fs,
    fs::read_dir,
    io::Write,
    path::{Path, PathBuf},
};
use toml::from_str;

#[derive(Deserialize, PartialEq, Debug)]
pub struct Config {
    pub settings: Settings,
    pub editors: Option<HashMap<String, String>>,
}

#[derive(Deserialize, PartialEq, Debug)]
pub struct Settings {
    pub default_editor: String,
    pub max_history_entries: usize,
    pub ls_display_block: usize,
}

#[derive(Debug)]
pub struct Env {
    pub config_file: PathBuf,
    pub database_file: PathBuf,
}

impl Env {
    fn read() -> Self {
        let mut home_dir = home_dir().unwrap_or(PathBuf::from("~/"));
        let config_dir = match var("HOP_CONFIG_DIRECTORY") {
            Ok(loc) => PathBuf::from(&loc),
            Err(_) => {
                home_dir.push(".config");
                home_dir.push("hop");
                home_dir
            }
        };
        let mut hop_config_file = PathBuf::from(&config_dir);
        match var("HOP_CONFIG_FILE_NAME") {
            Ok(name) => hop_config_file.push(name),
            Err(_) => hop_config_file.push("hop.toml"),
        };
        let mut database_dir = match var("HOP_DATABASE_DIRECTORY") {
            Ok(loc) => PathBuf::from(&loc),
            Err(_) => {
                let mut db_dir_temp =
                    PathBuf::from(&format!("{}", &config_dir.as_path().display().to_string()));
                db_dir_temp.push("db");
                db_dir_temp
            }
        };
        if !Path::new(&database_dir).exists() {
            match fs::create_dir_all(&database_dir) {
                Ok(_) => {}
                Err(e) => println!("[error] Error creating database directory: {}", e),
            };
        };
        match var("HOP_DATABASE_FILE_NAME") {
            Ok(name) => database_dir.push(name),
            Err(_) => database_dir.push("hop.sqlite"),
        };

        Env {
            config_file: hop_config_file,
            database_file: database_dir,
        }
    }
}
// Suppressing assignment warnings as functionality that uses `config` will be added in the future.
#[allow(dead_code)]
pub struct Hopper {
    pub config: Config,
    pub env: Env,
    pub db: sqlite::Connection,
}

impl Hopper {
    pub fn new() -> anyhow::Result<Self> {
        let env = Env::read();
        if !env.config_file.exists() {
            fs::create_dir_all(
                env.config_file
                    .parent()
                    .expect("[error] Unable to create config directory."),
            )
            .expect("[error] Unable to create config directory.");
            let mut new_conf =
                fs::File::create(&env.config_file).expect("[error] Unable to create config file.");
            new_conf
                .write_all(b"[settings]\ndefault_editor=\"nvim\"\nmax_history_entries=200\nls_display_block=0")
                .expect("[error] Unable to generate default config file.");
        };
        let toml_str: String = fs::read_to_string(env.config_file.clone()).unwrap();
        let configs: Config =
            from_str(&toml_str).expect("[error] Unable to parse configuration TOML.");
        let conn = sqlite::open(&env.database_file)?;
        conn.execute(
            "CREATE TABLE IF NOT EXISTS named_hops (
            name TEXT PRIMARY KEY,
            location TEXT NOT NULL
            )",
        )?;
        conn.execute(
            "CREATE TABLE IF NOT EXISTS history (
            time TEXT,
            name TEXT NOT NULL unique,
            location TEXT NOT NULL
            )",
        )?;

        Ok(Hopper {
            config: configs,
            env: env,
            db: conn,
        })
    }

    pub fn add_hop<T: AsRef<Path>>(&mut self, path: T, name: &str) -> anyhow::Result<()> {
        let query = format!(
            "INSERT OR REPLACE INTO named_hops (name, location) VALUES (\"{}\", \"{}\")",
            name,
            path.as_ref().display().to_string()
        );
        self.db.execute(&query)?;
        println!("[info] Hop created for {}.", name);
        Ok(())
    }

    pub fn remove_hop(&mut self, bunny: args::Rabbit) -> anyhow::Result<()> {
        let output_pair = match bunny {
            args::Rabbit::RequestName(name) => {
                Some((format!("name=\"{}\"", name), format!("shortcut: {}", name)))
            }
            args::Rabbit::RequestPath(loc) => Some((
                format!("location=\"{}\"", loc.as_path().display().to_string()),
                format!("location: {}", loc.as_path().display().to_string()),
            )),
            _ => None,
        };
        match output_pair {
            Some((q, n)) => match self
                .db
                .execute(&format!("DELETE FROM named_hops WHERE {}", q))
            {
                Ok(_) => println!("[info] Hop removed for {}.", n),
                Err(e) => println!("[error] Unable to remove {}, for error: {}", n, e),
            },
            None => println!("[error] Unable to determine hop to remove."),
        };
        Ok(())
    }

    pub fn use_hop(&mut self, shortcut_name: String) -> anyhow::Result<()> {
        let query = format!(
            "SELECT location FROM named_hops WHERE name=\"{}\"",
            &shortcut_name
        );
        let mut statement = self.db.prepare(&query)?;
        while let Ok(sqlite::State::Row) = statement.next() {
            let location = statement.read::<String, _>("location")?;
            let location_path = PathBuf::from(&location);
            if location_path.is_file() {
                println!(
                    "__cmd__ {} {}",
                    self.config.settings.default_editor, location
                );
            } else {
                println!("__cd__ {}", location);
            }
            return Ok(());
        }

        match self.check_dir(&shortcut_name) {
            Some((dir, short)) => {
                self.log_history(dir.as_path().display().to_string(), short)?;
                if dir.is_file() {
                    let ext_option = dir.extension();
                    let editor = match &self.config.editors {
                        Some(editor_map) => match ext_option {
                            Some(ext) => match editor_map.get(
                                &(ext
                                    .to_str()
                                    .expect("[error] Cannot extract extension.")
                                    .to_string()),
                            ) {
                                Some(special_editor) => special_editor,
                                None => &self.config.settings.default_editor,
                            },
                            None => &self.config.settings.default_editor,
                        },
                        None => &self.config.settings.default_editor,
                    };
                    println!("__cmd__ {} {}", editor, dir.as_path().display().to_string());
                } else {
                    println!("__cd__ {}", dir.as_path().display().to_string());
                };
                Ok(())
            }
            None => {
                println!("[error] Unable to find referenced file or directory.");
                Ok(())
            }
        }
    }

    pub fn just_do_it(&mut self, bunny: Rabbit) -> anyhow::Result<()> {
        match bunny {
            Rabbit::File(hop_name, hop_path) => self.add_hop(hop_path, &hop_name),
            Rabbit::Dir(hop_name, hop_path) => self.add_hop(hop_path, &hop_name),
            Rabbit::RequestName(shortcut_name) => self.use_hop(shortcut_name),
            Rabbit::RequestPath(_) => Ok(()),
        }
    }

    pub fn log_history(&self, location: String, name: String) -> anyhow::Result<()> {
        let query = format!(
            "INSERT INTO history (time, name, location) VALUES ({}, \"{}\", \"{}\") ",
            Local::now().format("%Y%m%d%H%M%S"),
            name,
            location
        );
        self.db.execute(&query)?;
        Ok(())
    }

    pub fn check_dir(&self, name: &str) -> Option<(PathBuf, String)> {
        read_dir(current_dir().unwrap())
            .expect("[error] Unable to search contents of current directory.")
            .filter(|f| f.is_ok())
            .map(|f| f.unwrap().path().to_path_buf())
            .map(|f| {
                (
                    f.clone(),
                    f.file_name()
                        .expect("[error] Unable to disambiguate file/directory.")
                        .to_str()
                        .expect("[error] Unable to convert file/directory name to UTF-8.")
                        .to_string(),
                )
            })
            .find(|(_, path_end)| path_end == name)
    }

    pub fn list_hops(&self) -> anyhow::Result<()> {
        let query = format!("SELECT name, location FROM named_hops");
        let mut query_result = self.db.prepare(&query)?;
        let mut hops: Vec<(String, String)> = Vec::new();
        while let Ok(sqlite::State::Row) = query_result.next() {
            let name = query_result.read::<String, _>("name")?;
            let location = query_result.read::<String, _>("location")?;
            hops.push((name, location));
        }
        let max_name_size = hops.iter().map(|(name, _)| name.len()).max().unwrap_or(0);
        let mut formatted_hops: Vec<String> = hops
            .into_iter()
            .map(|(name, location)| {
                (
                    String::from_utf8(vec![b' '; max_name_size - name.len() + 1])
                        .unwrap_or(" ".to_string()),
                    name,
                    location,
                )
            })
            .map(|(ws, name, location)| {
                format!(
                    "{}{}{} {}",
                    name.bold().cyan(),
                    ws,
                    "->".bright_white().bold(),
                    location.green().bold()
                )
            })
            .collect();
        formatted_hops.sort();
        for (idx, hop) in formatted_hops.into_iter().enumerate() {
            println!("{}", hop);
            if (self.config.settings.ls_display_block != 0)
                && ((idx + 1) % self.config.settings.ls_display_block == 0)
            {
                press_btn_continue::wait(&format!("{}\n", "Press_any_key_to_continue...".dimmed()))
                    .expect("[error] User input failed.");
            }
        }
        Ok(())
    }

    pub fn hop_names(&self) -> anyhow::Result<Vec<String>> {
        let query = format!("SELECT name FROM named_hops");
        let mut query_result = self.db.prepare(&query)?;
        let mut hops: Vec<String> = Vec::new();
        while let Ok(sqlite::State::Row) = query_result.next() {
            let name = query_result.read::<String, _>("name")?;
            hops.push(name);
        }
        Ok(hops)
    }

    pub fn brb<T: AsRef<Path>>(&mut self, path: T) -> anyhow::Result<()> {
        self.add_hop(path.as_ref(), "back")?;
        Ok(())
    }

    pub fn print_help() -> anyhow::Result<()> {
        println!(
            r#"
{} {} {}
    1) First argument is required.
    2) Second argument is optional.

Valid first argument commands are:
    1) {}: command to add a shortcut to the current directory.
        If a second argument is given, that argument is the name that will
        be used to refer to the shortcut for future use.
        If no second argument is given, the high level name will be used.
    2) {} or {}: command to list the current shortcuts and their names.
    3) {} or {}: both commands to show current hop version info.
    4) {}: command to create a temporary shortcut to the current directory
        that can be jumped back to using the {} {} command.
    5) {} or {}: command to remove the shortcut specified by {}.
    6) {}: Any other first arguments given will be checked to see if it
        represents a valid directory/file to hop to.  This input can be a named
        shortcut, a file/directory in the current directory, or a file/directory
        from previous {} commands."#,
            "hp".bold(),
            "arg1".italic().dimmed(),
            "arg2".italic().dimmed(),
            "add".cyan().bold(),
            "ls".cyan().bold(),
            "list".cyan().bold(),
            "v".cyan().bold(),
            "version".cyan().bold(),
            "brb".cyan().bold(),
            "hp".bold(),
            "back".italic().dimmed(),
            "rm".cyan().bold(),
            "remove".cyan().bold(),
            "arg2".italic().dimmed(),
            "_".cyan().bold(),
            "hp".bold()
        );
        Ok(())
    }

    pub fn runner(&self, cmd: String) -> anyhow::Result<()> {
        let bunnyhop_exe = current_exe()
            .expect("[error] Unable to extract current bunnyhop executable name.")
            .into_os_string()
            .to_str()
            .expect("[error] Unable to convert current bunnyhop executable path to UTF-8.")
            .to_string();
        println!("__cmd__ {} {}", bunnyhop_exe, cmd);
        Ok(())
    }

    pub fn execute(&mut self, cmd: Cmd) -> anyhow::Result<()> {
        match cmd {
            Cmd::Passthrough(cmd) => self.runner(cmd),
            Cmd::Use(bunny) => self.just_do_it(bunny),
            Cmd::SetBrb(loc) => self.brb(loc),
            Cmd::BrbHop => self.use_hop("back".to_string()),
            Cmd::ListHops => self.list_hops(),
            Cmd::PrintHelp => Self::print_help(),
            Cmd::Remove(bunny) => self.remove_hop(bunny),
            Cmd::PrintMsg(msg) => {
                println!("{}", msg);
                Ok(())
            }
        }
    }
}

impl Default for Hopper {
    fn default() -> Self {
        Self::new().expect("[error] Unable to create a hopper.")
    }
}
