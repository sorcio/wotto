use std::sync::Arc;

use futures::prelude::*;
use irc::client::prelude::*;
use rusto_utils::debug::debug_arc;
use warp::Filter;

use crate::parsing;

pub async fn bot_main() -> Result<(), Box<dyn std::error::Error>> {
    let config = Config::load("rusto.toml")?;

    let rustico = rustico::Service::new();

    let join_handles = {
        let mut join_handles = vec![];

        let state = Arc::new(BotState::new(config, rustico));

        let web_task = tokio::spawn({
            let state = Arc::downgrade(&state);
            async { web_server(state).await }
        });

        let epoch_timer = std::thread::spawn({
            let state = Arc::downgrade(&state);
            move || loop {
                std::thread::sleep(std::time::Duration::from_millis(50));
                let Some(state) = state.upgrade() else { break; };
                state.rustico().increment_epoch();
            }
        });
        join_handles.push(epoch_timer);

        let ctrl_c_task = tokio::spawn(ctrl_c_monitor(Arc::downgrade(&state)));

        let _ = state.clone().irc_task().await;
        eprintln!("irc_task quit");

        ctrl_c_task.abort();

        // TODO close web task cleanly?
        eprintln!("shutting down web server...");
        let _ = tokio::time::timeout(std::time::Duration::from_millis(500), web_task).await;

        // state must have zero strong references at this point
        #[cfg(debug_assertions)]
        {
            eprintln!("irc state: {}", debug_arc(&state));
        }

        join_handles
    };

    eprintln!("shutting down epoch timer...");
    for handle in join_handles {
        let _ = handle.join();
    }

    eprintln!("all done, bye!");

    Ok(())
}

async fn ctrl_c_monitor(state: std::sync::Weak<BotState>) {
    let Ok(_) = tokio::signal::ctrl_c().await else { return; };
    if let Some(state) = state.upgrade() {
        eprintln!("received Ctrl-C; requesting quit");
        state.request_quit();
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UserMask {
    nick: String,
    user: String,
    host: String,
}

impl TryFrom<irc::proto::Prefix> for UserMask {
    type Error = ();

    fn try_from(value: irc::proto::Prefix) -> Result<Self, Self::Error> {
        match value {
            Prefix::ServerName(_) => Err(()),
            Prefix::Nickname(nick, user, host) => Ok(Self { nick, user, host }),
        }
    }
}

impl std::str::FromStr for UserMask {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        use parsing::user_prefix;
        let (nick, user, host) = user_prefix(s).map_err(|_| ())?;
        Ok(Self {
            nick: nick.to_string(),
            user: user.to_string(),
            host: host.to_string(),
        })
    }
}

mod state {
    use std::fmt::Debug;
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    use irc::client::prelude::Config;
    use irc::client::Client;
    use irc::proto::Prefix;
    use tokio::sync::{AcquireError, RwLock, Semaphore};

    use super::{BotCommand, CommandName, UserMask};
    use crate::throttling::Throttler;

    struct TrustedUsers {
        list: Vec<UserMask>,
    }

    impl TrustedUsers {
        fn from_config(config: &Config) -> Self {
            let list = match config.get_option("default_trust") {
                Some(prefix) => match prefix.parse() {
                    Ok(prefix) => vec![prefix],
                    Err(_) => {
                        eprintln!("warning: default_trust cannot be parsed!");
                        vec![]
                    }
                },
                None => {
                    eprintln!("warning: no default_trust option specified");
                    vec![]
                }
            };
            Self { list }
        }
    }

    impl TrustedUsers {
        fn is_trusted(&self, mask: &UserMask) -> bool {
            self.list.iter().any(|x| x == mask)
        }

        fn is_trusted_prefix(&self, prefix: Option<Prefix>) -> bool {
            if let Some(prefix) = prefix {
                if let Ok(other_mask) = prefix.try_into() {
                    self.is_trusted(&other_mask)
                } else {
                    false
                }
            } else {
                false
            }
        }

        fn add_trust(&mut self, mask: &UserMask) -> bool {
            if self.is_trusted(mask) {
                false
            } else {
                self.list.push(mask.clone());
                true
            }
        }

        fn iter(&self) -> impl Iterator<Item = &UserMask> {
            self.list.iter()
        }
    }

    async fn check_trust(state: &BotState, prefix: Option<Prefix>) -> bool {
        state.trusted.read().await.is_trusted_prefix(prefix)
    }

    pub(crate) struct BotState {
        config: Config,
        client: RwLock<Option<Client>>,
        rustico: rustico::Service,
        trusted: RwLock<TrustedUsers>,
        throttler: Throttler,
        engine_semaphore: Semaphore,
        quitting: AtomicBool,
    }

    impl BotState {
        pub(crate) fn new(config: Config, rustico: rustico::Service) -> Self {
            let throttler = Throttler::make()
                .layer(5, 2500)
                .layer(2, 150)
                .layer(1, 50)
                .build();
            let engine_semaphore = Semaphore::new(2);
            let trusted = TrustedUsers::from_config(&config);
            Self {
                config,
                client: RwLock::new(None),
                rustico,
                trusted: RwLock::new(trusted),
                throttler,
                engine_semaphore,
                quitting: AtomicBool::new(false),
            }
        }

        pub(crate) fn client<F, T>(&self, f: F) -> Option<T>
        where
            F: FnOnce(&Client) -> T,
        {
            match self.client.try_read() {
                Ok(guard) => guard.as_ref().map(f),
                Err(_) => None,
            }
        }

        pub(crate) fn rustico(&self) -> &rustico::Service {
            &self.rustico
        }

        pub(crate) async fn management_command(
            slf: Arc<Self>,
            source: Option<Prefix>,
            response_target: String,
            cmd: &BotCommand,
        ) {
            match cmd.command() {
                CommandName::Plain(x) if x == "ping" => {
                    slf.reply(response_target, "pong").await;
                }
                CommandName::Plain(x) if x == "join" => {
                    if !check_trust(&slf, source).await {
                        return;
                    }
                    let chans: Vec<_> = cmd.args.split_whitespace().collect();
                    let _ = slf.client(|client| client.send_join(chans.join(",")));
                }
                CommandName::Plain(x) if x == "trust" => {
                    if !check_trust(&slf, source).await {
                        return;
                    }
                    if let Ok(mask) = cmd.args().trim().parse() {
                        let mut trusted = slf.trusted.write().await;
                        let message = if trusted.add_trust(&mask) {
                            format!("I now trust {}", mask.nick)
                        } else {
                            format!("I already trust {}", mask.nick)
                        };
                        slf.reply(response_target, message).await;
                    } else {
                        eprintln!("invalid prefix: {:?}", cmd.args());
                    }
                }
                CommandName::Plain(x) if x == "untrust" => {
                    if !check_trust(&slf, source).await {
                        return;
                    }
                    let mut trusted = slf.trusted.write().await;
                    *trusted = TrustedUsers::from_config(&slf.config);
                    eprintln!("trusted list reset");
                }
                CommandName::Plain(x) if x == "trust-list" => {
                    if !check_trust(&slf, source).await {
                        return;
                    }
                    let trusted = slf.trusted.read().await;
                    eprintln!("Trusted list:");
                    for p in trusted.iter() {
                        eprintln!(" * {p:?}");
                    }
                }
                CommandName::Plain(x) if x == "load" => {
                    if !check_trust(&slf, source).await {
                        return;
                    }
                    let module_name = cmd.args.trim().to_string();
                    let state = slf.clone();
                    tokio::spawn(async move {
                        let response = match state.rustico().load_module(module_name).await {
                            Ok(name) => format!("loaded module: {name}"),
                            Err(error) => {
                                eprintln!("management load failed: {error}");
                                "cannot load module (check logs)".to_string()
                            }
                        };
                        state.reply(response_target, response).await;
                    });
                }
                CommandName::Plain(x) if x == "permits" => {
                    if !check_trust(&slf, source).await {
                        return;
                    }
                    let available_permits = slf.engine_semaphore.available_permits();
                    slf.reply(
                        response_target,
                        format!("available permits: {available_permits}"),
                    )
                    .await;
                }
                CommandName::Plain(x) if x == "quit" => {
                    if !check_trust(&slf, source).await {
                        return;
                    }
                    slf.request_quit();
                }
                _ => {
                    eprintln!("not a valid management command: {cmd:?}");
                }
            }
        }

        #[tracing::instrument]
        pub(crate) async fn reply<R: AsRef<str> + Debug, M: AsRef<str> + Debug>(
            &self,
            response_target: R,
            message: M,
        ) {
            const MAX_SIZE: usize = 512;
            let target = response_target.as_ref();
            let message = message.as_ref();

            for (i, line) in message
                .split_terminator(|c| c == '\r' || c == '\n')
                .filter(|x| !x.is_empty())
                .enumerate()
            {
                let prefix = if i == 0 { "\x02>\x0f" } else { "\x02:\x0f" };
                let line = format!("{prefix}{line}");
                let overhead = target.bytes().len() + b"PRIVMSG   :\r\n".len();
                let max_payload_size = MAX_SIZE.saturating_sub(overhead);
                let boundary = line.floor_char_boundary(max_payload_size);
                self.throttler.acquire_one().await;
                let _ = self.client(|client| client.send_privmsg(target, &line[..boundary]));
            }
        }

        pub(crate) async fn engine_permit(&self) -> Result<impl Drop + '_, AcquireError> {
            self.engine_semaphore.acquire().await
        }

        pub(crate) async fn irc_task(self: Arc<Self>) -> Result<(), irc::error::Error> {
            while !self.quitting.load(std::sync::atomic::Ordering::SeqCst) {
                eprintln!("starting new client...");
                let mut client = Client::from_config(self.config.clone()).await?;
                client.identify()?;
                let stream = client.stream()?;
                *self.client.write().await = Some(client);
                match super::irc_stream_handler(stream, self.clone()).await {
                    Ok(_) => {}
                    Err(error) => {
                        eprintln!("irc stream loop terminated with error: {error}");
                    }
                }
            }
            Ok(())
        }

        pub(crate) fn request_quit(&self) {
            let already_quitting = self.quitting
                .swap(true, std::sync::atomic::Ordering::SeqCst);
            if !already_quitting {
                let _ = self.client(|client| client.send_quit("requested"));
            }
        }
    }

    impl core::fmt::Debug for BotState {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("BotState").field("quitting", &self.quitting).finish()
    }
    }
}

use state::BotState;

async fn irc_stream_handler(
    mut stream: irc::client::ClientStream,
    state: Arc<BotState>,
) -> Result<(), Box<dyn std::error::Error>> {
    // let client = state.client();
    while let Some(message) = stream.next().await.transpose()? {
        println!("\x1b[2m{}\x1b[0m", message.to_string().trim_end());
        // let prefixes = [
        //     "!",
        //     &format!("{} ", client.current_nickname()),
        //     &format!("{}:", client.current_nickname()),
        //     &format!("{}!", client.current_nickname()),
        // ];
        #[allow(clippy::single_match)]
        match message.command {
            Command::PRIVMSG(_, ref text) => {
                if let Ok(cmd) = BotCommand::parse(&[], text) {
                    eprintln!("got cmd {cmd:?}");
                    let Some(response_target) = message.response_target().map(str::to_owned) else { break; };
                    let w = Arc::downgrade(&state);
                    handle_command(
                        message.prefix,
                        response_target.clone(),
                        cmd,
                        state.clone(),
                        move |response| async move {
                            if let Some(state) = w.upgrade() {
                                state.reply(response_target, response).await;
                            }
                        },
                    );
                }
            }
            _ => {}
        }
    }

    Ok(())
}

fn handle_command<F, Fut>(
    source: Option<irc::proto::Prefix>,
    response_target: String,
    cmd: BotCommand,
    state: Arc<BotState>,
    handler: F,
) where
    F: FnOnce(String) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = ()> + Send,
{
    let args = cmd.args().to_string();
    let (module_name, entry_point) = match cmd.command() {
        CommandName::Plain(_) => {
            tokio::spawn(async move {
                BotState::management_command(state, source, response_target, &cmd).await;
            });
            return;
        }
        CommandName::Namespaced(ns, name) => (ns.to_string(), name.to_string()),
    };
    let task_name = format!("command::{module_name}::{entry_point}");
    let run_task = tokio::task::Builder::new().name(&task_name);
    run_task.spawn(async move {
        let Ok(permit) = state.engine_permit().await else { return; };
        match state
            .rustico()
            .run_module(&module_name, &entry_point, &args)
            .await
        {
            Ok(s) => handler(s).await,
            Err(rustico::Error::TimedOut) => {
                // TODO irc code shouldn't be mixed here I think
                state
                    .reply(
                        response_target,
                        format!(
                            "{} is taking too long to execute and has been interrupted.",
                            cmd.command()
                        ),
                    )
                    .await;
            }
            Err(err) => {
                eprintln!("error on command: {err}");
            }
        }
        // being super-explicit that engine permit is released only after the
        // whole response has been sent out:
        drop(permit);
    }).unwrap();
}

struct ParseError;

#[derive(Debug, Clone)]
pub(crate) enum CommandName {
    Plain(String),
    Namespaced(String, String),
}

impl std::fmt::Display for CommandName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CommandName::Plain(x) => f.write_str(x),
            CommandName::Namespaced(ns, x) => write!(f, "{ns}.{x}"),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct BotCommand {
    // prefix: String,
    pub(crate) command: CommandName,
    pub(crate) args: String,
}

impl BotCommand {
    fn parse(_prefixes: &[&str], text: &str) -> Result<Self, ParseError> {
        parsing::command(text).map_err(|_| ParseError)
    }

    pub(crate) fn command(&self) -> &CommandName {
        &self.command
    }

    pub(crate) fn args(&self) -> &str {
        &self.args
    }
}

async fn web_server(state: std::sync::Weak<BotState>) {
    // GET /hello/warp => 200 OK with body "Hello, warp!"
    let hello = warp::path!("hello" / String).map(|name| format!("Hello, {}!", name));
    let load_module = warp::path!("load" / String)
        .and(warp::post())
        .then({
            let state = state.clone();
            move |module: String| {
                let state = state.clone();
                async move {
                    let Some(state) = state.upgrade() else { return; };
                    match state.rustico().load_module(module.clone()).await {
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
            let state = state.clone();
            move |chan_type: String, chan_name: String| {
                let chan_name = if chan_type == "hash" {
                    format!("#{chan_name}")
                } else {
                    format!("{chan_type}{chan_name}")
                };
                let state = state.clone();
                async move {
                    let Some(state) = state.upgrade() else { return; };
                    match state.client(|client| client.send_join(&chan_name)) {
                        Some(Ok(_)) => {}
                        Some(Err(err)) => eprintln!("cannot join channel {chan_name}: {err}"),
                        None => {}
                    }
                }
            }
        })
        .map(|_| "");

    let filter: _ = hello.or(load_module).or(join_channel);

    warp::serve(filter).run(([127, 0, 0, 1], 3030)).await;
}
