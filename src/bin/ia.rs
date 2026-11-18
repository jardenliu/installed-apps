//! Example CLI for the `installed-apps` library.
//!
//! Usage:
//!   ia list [--json]           list all installed applications
//!   ia assoc <ext> [--json]    apps that can open *.<ext> (e.g. `ia assoc html`)
//!   ia app <name> [--json]     details of one application by name

use installed_apps::{AppInfo, Scanner};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let json = args.iter().any(|a| a == "--json");
    let positional: Vec<&str> = args
        .iter()
        .filter(|a| !a.starts_with("--"))
        .map(|s| s.as_str())
        .collect();

    let scanner = Scanner::new();
    let result = match positional.as_slice() {
        ["list"] => print_apps(scanner.scan(), json),
        ["assoc", ext] => print_apps(scanner.apps_for_extension(ext), json),
        ["app", name] => match scanner.find_app(name) {
            Ok(Some(app)) => print_apps(Ok(vec![app]), json),
            Ok(None) => {
                eprintln!("no application matching {name:?}");
                std::process::exit(1);
            }
            Err(e) => Err(e),
        },
        _ => {
            eprintln!("usage: ia list [--json] | ia assoc <ext> [--json] | ia app <name> [--json]");
            std::process::exit(2);
        }
    };
    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn print_apps(apps: installed_apps::Result<Vec<AppInfo>>, json: bool) -> installed_apps::Result<()> {
    let apps = apps?;
    if json {
        println!("{}", serde_json::to_string_pretty(&apps).expect("serializable"));
        return Ok(());
    }
    if apps.is_empty() {
        println!("(no results)");
        return Ok(());
    }
    for app in apps {
        println!("{}", app.name);
        println!("  id:       {}", app.id);
        println!("  path:     {}", app.install_path.display());
        if let Some(exe) = &app.executable {
            println!("  exe:      {}", exe.display());
        }
        println!("  open:     {}", app.launch.template());
        match &app.icon {
            Some(icon) => {
                // Truncate the base64 blob; full value is in --json output.
                let mut b64 = icon.base64.clone();
                b64.truncate(48);
                println!(
                    "  icon:     {}x{} PNG data:image/png;base64,{b64}…",
                    icon.width, icon.height
                );
            }
            None => println!("  icon:     (none)"),
        }
        if !app.extensions.is_empty() {
            println!("  exts:     {}", app.extensions.join(", "));
        }
        println!();
    }
    Ok(())
}
