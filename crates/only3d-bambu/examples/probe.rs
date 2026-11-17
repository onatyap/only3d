//! Manual probe for a real printer.
//!
//! ```text
//! cargo run -p only3d-bambu --example probe -- discover [seconds]
//! cargo run -p only3d-bambu --example probe -- status
//! cargo run -p only3d-bambu --example probe -- files
//! cargo run -p only3d-bambu --example probe -- upload <local file> [remote name]
//! cargo run -p only3d-bambu --example probe -- print <remote name> [--plate N] [--ams 0,1,..] [--timelapse] --yes
//! ```
//!
//! Everything except `discover` reads the printer from the environment:
//! `ONLY3D_PRINTER_HOST`, `ONLY3D_PRINTER_SERIAL`,
//! `ONLY3D_PRINTER_ACCESS_CODE`, optional `ONLY3D_PRINTER_CERT_SHA256`
//! and `ONLY3D_PRINTER_MODEL` (e.g. `O1D`).

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use only3d_bambu::{Error, PrintJob, PrinterClient, PrinterConfig};

fn env_config() -> Result<PrinterConfig, Error> {
    let var = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
    let missing = |k: &str| Error::InvalidConfig {
        detail: format!("environment variable {k} is not set"),
    };
    let mut cfg = PrinterConfig::new(
        var("ONLY3D_PRINTER_HOST").ok_or_else(|| missing("ONLY3D_PRINTER_HOST"))?,
        var("ONLY3D_PRINTER_SERIAL").ok_or_else(|| missing("ONLY3D_PRINTER_SERIAL"))?,
        var("ONLY3D_PRINTER_ACCESS_CODE").ok_or_else(|| missing("ONLY3D_PRINTER_ACCESS_CODE"))?,
    );
    cfg.cert_sha256 = var("ONLY3D_PRINTER_CERT_SHA256");
    cfg.model = var("ONLY3D_PRINTER_MODEL");
    cfg.validate()?;
    Ok(cfg)
}

fn usage() -> ExitCode {
    eprintln!(
        "usage: probe discover [seconds] | status | files | upload <file> [remote] | \
         print <remote> [--plate N] [--ams 0,1] [--timelapse] --yes"
    );
    ExitCode::from(2)
}

fn run(args: &[String]) -> Result<(), Error> {
    match args.first().map(String::as_str) {
        Some("discover") => {
            let secs: u64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(8);
            eprintln!("listening {secs} s on UDP 2021/1990 ...");
            let report = only3d_bambu::discover_detailed(Duration::from_secs(secs))?;
            for w in &report.warnings {
                eprintln!("warning: {w}");
            }
            if report.printers.is_empty() {
                eprintln!(
                    "no printers answered (listened on UDP {:?}; check the LAN, the macOS firewall, and that \
                     Bambu Studio is not holding UDP 2021)",
                    report.listening_ports
                );
            }
            for p in &report.printers {
                println!(
                    "{}  {}  model={} ({})  fw={}  name={}  connect={}  bind={}  signal={}",
                    p.serial,
                    p.ip,
                    p.model_code.as_deref().unwrap_or("?"),
                    p.model_name.as_deref().unwrap_or("unknown model"),
                    p.firmware.as_deref().unwrap_or("?"),
                    p.name.as_deref().unwrap_or("?"),
                    p.connect_mode.as_deref().unwrap_or("?"),
                    p.bind_state.as_deref().unwrap_or("?"),
                    p.signal.as_deref().unwrap_or("?"),
                );
            }
            Ok(())
        }
        Some("status") => {
            let cfg = env_config()?;
            let client = PrinterClient::connect(&cfg)?;
            let _ = client.wait_for(|s| s.product_name.is_some(), Duration::from_secs(3));
            println!("{}", serde_json::to_string_pretty(&client.status())?);
            if cfg.cert_sha256.is_none() {
                eprintln!(
                    "certificate sha256 (pin with ONLY3D_PRINTER_CERT_SHA256): {}",
                    client.tls_fingerprint().unwrap_or_default()
                );
            }
            client.disconnect();
            Ok(())
        }
        Some("files") => {
            let cfg = env_config()?;
            for f in only3d_bambu::list_files(&cfg)? {
                println!(
                    "{}{:>12}  {}  {}",
                    if f.is_dir { "d" } else { "-" },
                    f.size.map(|s| s.to_string()).unwrap_or_default(),
                    f.modified.unwrap_or_default(),
                    f.name
                );
            }
            Ok(())
        }
        Some("upload") => {
            let local = PathBuf::from(args.get(1).ok_or_else(|| Error::InvalidInput {
                detail: "upload needs a local file".into(),
            })?);
            let remote = match args.get(2) {
                Some(r) => r.clone(),
                None => local
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(str::to_string)
                    .ok_or_else(|| Error::InvalidInput {
                        detail: "cannot derive a remote name".into(),
                    })?,
            };
            let cfg = env_config()?;
            let mut last = 0;
            let mut progress = |sent: u64, total: Option<u64>| {
                let pct = total.map(|t| sent * 100 / t.max(1)).unwrap_or(0);
                if pct >= last + 5 || Some(sent) == total {
                    eprintln!("  {sent} / {} bytes ({pct}%)", total.unwrap_or(0));
                    last = pct;
                }
                true
            };
            let res = only3d_bambu::upload_file(&cfg, &local, &remote, Some(&mut progress))?;
            println!("{}", serde_json::to_string_pretty(&res)?);
            Ok(())
        }
        Some("print") => {
            let remote = args.get(1).filter(|a| !a.starts_with("--")).ok_or_else(|| Error::InvalidInput {
                detail: "print needs the remote file name".into(),
            })?;
            if !args.iter().any(|a| a == "--yes") {
                return Err(Error::InvalidInput {
                    detail: format!(
                        "refusing to start printing {remote:?} without --yes (this heats the printer and moves it)"
                    ),
                });
            }
            let mut job = PrintJob::new(remote.clone());
            let mut it = args.iter().skip(2);
            while let Some(a) = it.next() {
                match a.as_str() {
                    "--plate" => {
                        job.plate = it.next().and_then(|v| v.parse().ok()).ok_or_else(|| Error::InvalidInput {
                            detail: "--plate needs a number".into(),
                        })?
                    }
                    "--ams" => {
                        job.ams_mapping = it
                            .next()
                            .map(|v| v.split(',').map(|x| x.trim().parse::<i32>()).collect::<Result<Vec<_>, _>>())
                            .and_then(Result::ok)
                            .ok_or_else(|| Error::InvalidInput {
                                detail: "--ams needs comma-separated tray ids, e.g. 0,5".into(),
                            })?
                    }
                    "--timelapse" => job.timelapse = true,
                    "--no-ams" => job.use_ams = false,
                    "--yes" => {}
                    other => {
                        return Err(Error::InvalidInput {
                            detail: format!("unknown flag {other}"),
                        })
                    }
                }
            }
            let cfg = env_config()?;
            let client = PrinterClient::connect(&cfg)?;
            let ack = only3d_bambu::start_print(&client, &job)?;
            println!("{}", serde_json::to_string_pretty(&ack)?);
            client.disconnect();
            Ok(())
        }
        _ => Err(Error::InvalidInput {
            detail: "unknown command".into(),
        }),
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        return usage();
    }
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error[{}]: {e}\nhint: {}", e.code(), e.hint());
            ExitCode::FAILURE
        }
    }
}
