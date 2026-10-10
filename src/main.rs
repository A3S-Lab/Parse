//! `a3s-parse serve` listens for `POST /v1/parse`.

use std::env;
use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
    let mut args = env::args().skip(1);
    match args.next().as_deref() {
        Some("serve") => {
            let mut listen = "127.0.0.1:8091".to_string();
            while let Some(arg) = args.next() {
                if arg == "--listen" {
                    match args.next() {
                        Some(value) => listen = value,
                        None => {
                            eprintln!("--listen requires host:port");
                            return ExitCode::from(2);
                        }
                    }
                } else {
                    eprintln!("unknown argument {arg}");
                    return ExitCode::from(2);
                }
            }
            match a3s_parse::serve_native(&listen).await {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    eprintln!("{error}");
                    ExitCode::from(1)
                }
            }
        }
        _ => {
            eprintln!("usage: a3s-parse serve [--listen host:port]");
            ExitCode::from(2)
        }
    }
}
