use std::sync::Arc;

use futures::prelude::*;
use irc::client::prelude::*;
use warp::Filter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = Config::load("rusto.toml")?;

    let svc = rustico::Service::new();
    let rustico = Arc::new(svc);
    // let (tx, rx) = mpsc::channel(32);
    // let rustico_task = tokio::spawn(async move {
    //     svc.listen(rx).await;
    // });

    let mut client = Client::from_config(config).await?;
    client.identify()?;

    let stream = client.stream()?;

    let client = Arc::new(client);

    let irc_task = tokio::spawn({
        let rustico = rustico.clone();
        let client = client.clone();
        async move { irc_stream_handler(stream, client, rustico).await.unwrap() }
    });

    let web_task = tokio::spawn({
        let rustico = rustico.clone();
        let client = client.clone();
        async { web_server(rustico, client).await }
    });

    let _ = tokio::join!(web_task, irc_task);

    Ok(())
}

async fn irc_stream_handler(
    mut stream: irc::client::ClientStream,
    client: Arc<Client>,
    rustico: Arc<rustico::Service>,
) -> Result<(), Box<dyn std::error::Error>> {
    while let Some(message) = stream.next().await.transpose()? {
        println!("{:?}", message);
        #[allow(clippy::single_match)]
        match message.command {
            Command::PRIVMSG(_, ref text) => {
                if let Some(args) = text.strip_prefix("!add ") {
                    let Some(response_target) = message.response_target().map(str::to_owned) else { break; };
                    let args = args.to_string();
                    let s = rustico.clone();
                    let client = Arc::downgrade(&client);
                    tokio::spawn(async move {
                        match s
                            .run_module("math.wasm".to_string(), "add".to_string(), args)
                            .await
                        {
                            Ok(s) => {
                                client
                                    .upgrade()
                                    .map(|client| client.send_notice(response_target, s).ok());
                            }
                            Err(err) => {
                                eprintln!("error on command: {err}")
                            }
                        }
                    });
                }
            }
            _ => {}
        }
    }

    Ok(())
}

async fn web_server(rustico: Arc<rustico::Service>, client: Arc<Client>) {
    // GET /hello/warp => 200 OK with body "Hello, warp!"
    let hello = warp::path!("hello" / String).map(|name| format!("Hello, {}!", name));
    let load_module = warp::path!("load" / String)
        .and(warp::post())
        .then({
            let rustico = rustico.clone();
            move |module: String| {
                let rustico = rustico.clone();
                async move {
                    match rustico.load_module(module.clone()).await {
                        Ok(_) => eprintln!("loaded module {module}"),
                        Err(err) => eprintln!("cannot load module {module}: {err}"),
                    };
                }
            }
        })
        .map(|_| "");

    let join_channel = warp::path!("join" / String / String)
        .and(warp::post())
        .then({
            let client = client.clone();
            move |chan_type: String, chan_name: String| {
                let chan_name = if chan_type == "hash" {
                    format!("#{chan_name}")
                } else {
                    format!("{chan_type}{chan_name}")
                };
                let client = client.clone();
                async move {
                    match client.send_join(&chan_name) {
                        Ok(_) => {}
                        Err(err) => eprintln!("cannot join channel {chan_name}: {err}"),
                    }
                }
            }
        })
        .map(|_| "");

    let filter: _ = hello.or(load_module).or(join_channel);

    warp::serve(filter).run(([127, 0, 0, 1], 3030)).await;
}
