// Command parser

use anyhow::Result;
use rustyline::error::ReadlineError;
use rustyline::DefaultEditor;
use tokio::sync::mpsc;

use crate::service::{Command, Service, Result as RusticoResult};

pub async fn repl() -> Result<()> {
    let svc = Service::new();

    let (req_tx, req_rx) = mpsc::channel(1);
    let (resp_tx, resp_rx) = mpsc::channel(1);
    tokio::spawn(async move {
        svc.listen(req_rx, resp_tx).await;
    });

    // Tokio recommends to use normal blocking I/O for interactive stdin/out.
    // We do it in a thread so we don't have to mix blocking and async style.
    let (req_tx, mut resp_rx) = std::thread::scope(|s| {
        s.spawn(move || command_parser(req_tx, resp_rx)).join().unwrap()
    })?;

    println!("Shutting down rustico service...");
    let _ = req_tx.send(Command::Quit).await;
    resp_rx.close();
    while let Some(msg) = resp_rx.recv().await {
        println!("got {:?}", msg);
    }

    Ok(())
}

type ReqTx = mpsc::Sender<Command>;
type RespRx = mpsc::Receiver<RusticoResult<String>>;

fn command_parser(tx: ReqTx, mut resp: RespRx) -> Result<(ReqTx, RespRx)> {
    println!("rustico CLI {}", option_env!("CARGO_PKG_VERSION").unwrap_or("dev"));
    let mut rl = DefaultEditor::new()?;
    loop {
        let readline = rl.readline(">> ");
        match readline {
            Ok(line) if line.is_empty() => {}
            Ok(line) => {
                let _ = rl.add_history_entry(line.as_str());
                if let Some(cmd) = parse_command(line) {
                    tx.blocking_send(cmd)?;
                    match resp.blocking_recv() {
                        Some(Ok(response)) => println!("++ {response}"),
                        Some(Err(error)) => println!("!! {error}"),
                        None => { break; }
                    }
                } else {
                    println!("Cannot parse input.")
                }
            }
            Err(ReadlineError::Interrupted) => {
                break;
            }
            Err(ReadlineError::Eof) => {
                break;
            }
            Err(err) => {
                println!("Error: {:?}", err);
                break;
            }
        }
    }
    Ok((tx, resp))
}

fn parse_command(cmd: String) -> Option<Command> {
    let args: Vec<_> = cmd.split_whitespace().collect();
    match &args[..] {
        ["load", module] => Some(Command::LoadModule(module.to_string())),
        ["run", module, entry_point, ..] => Some(Command::RunModule {
            module: module.to_string(),
            entry_point: entry_point.to_string(),
            args: args[3..].join(" "),
        }),
        _ => None,
    }
}
