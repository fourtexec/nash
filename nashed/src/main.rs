//! nashed - package and version manager for the nash shell.

mod manager;
mod release;
mod sys;

const USAGE: &str = "\
usage: nashed <command> [options]

commands:
  install [dev | VERSION]     install nash (latest stable, dev, or a specific version)
  update [dev | VERSION]      update nash to the newest release or a chosen version
  update self [dev | VERSION] update nashed itself
  list [self]                 list releases available for this platform
  uninstall                   uninstall nash

options:
  -V, --version               print version and exit
  -h, --help                  print this help

Run `nashed <command> --help' for command-specific options.
";

/// Version of this build: `NASH_RELEASE` when built by CI, otherwise crate version.
pub fn version() -> &'static str {
    match option_env!("NASH_RELEASE") {
        Some(v) if !v.is_empty() => v.strip_prefix('v').unwrap_or(v),
        _ => env!("CARGO_PKG_VERSION"),
    }
}

fn main() {
    let args: Vec<String> = std::env::args_os()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let code = match args.get(1).map(|s| s.as_str()) {
        None | Some("-h") | Some("--help") => {
            print!("{}", USAGE);
            0
        }
        Some("-V") | Some("--version") => {
            println!("nashed {}", version());
            0
        }
        Some("install") => manager::cmd_install(&args[2..]),
        Some("update") => manager::cmd_update(&args[2..]),
        Some("uninstall") => manager::cmd_uninstall(&args[2..]),
        Some("list") => manager::cmd_list(&args[2..]),
        Some(other) => {
            eprintln!("nashed: unknown command `{}'\n\n{}", other, USAGE);
            2
        }
    };
    std::process::exit(code);
}
