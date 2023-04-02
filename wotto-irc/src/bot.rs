use std::sync::Arc;

use futures::future::join_all;
use futures::prelude::*;
use irc::client::prelude::*;
use tracing::{error, info, trace, warn};
use valuable::Valuable;
use warp::Filter;

use crate::parsing;

pub async fn bot_main() -> Result<(), Box<dyn std::error::Error>> {
    let config = Config::load("wotto.toml")?;

    let rustico = rustico::Service::new();

    let futures = {
        let mut futures = vec![];

        let state = Arc::new(BotState::new(config, rustico));

        let web_task = tokio::spawn({
            let state = Arc::downgrade(&state);
            async { web_server(state).await }
        });

        futures.push(state.engine_epoch_timer());

        let ctrl_c_task = tokio::spawn(ctrl_c_monitor(Arc::downgrade(&state)));

        let _ = state.clone().irc_task().await;
        trace!("irc_task quit");

        ctrl_c_task.abort();

        // TODO close web task cleanly?
        trace!("shutting down web server");
        let _ = tokio::time::timeout(std::time::Duration::from_millis(500), web_task).await;

        // state must have zero strong references at this point
        #[cfg(debug_assertions)]
        {
            use wotto_utils::debug::debug_arc;
            use tracing::debug;
            debug!("irc state: {}", debug_arc(&state));
        }

        futures
    };

    trace!("waiting for full shutdown");
    let _ = tokio::time::timeout(std::time::Duration::from_millis(1000), join_all(futures)).await;

    trace!("all done, bye!");

    Ok(())
}

async fn ctrl_c_monitor(state: std::sync::Weak<BotState>) {
    let Ok(_) = tokio::signal::ctrl_c().await else { return; };
    if let Some(state) = state.upgrade() {
        info!("received Ctrl-C; requesting quit");
        state.request_quit();
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UserMask {
    nick: String,
    user: String,
    host: String,
}

impl UserMask {
    pub(crate) fn new(nick: String, user: String, host: String) -> Self {
        Self { nick, user, host }
    }

    pub(crate) fn from_parts(nick: &str, user: &str, host: &str) -> Self {
        Self::new(nick.to_string(), user.to_string(), host.to_string())
    }

    pub(crate) fn prefix_length(&self) -> usize {
        self.nick.bytes().len() + 1 + self.user.bytes().len() + 1 + self.host.bytes().len()
    }
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
    use tracing::{error, info, trace};
    use valuable::Valuable;

    use super::{BotCommand, CommandName, UserMask};
    use crate::throttling::Throttler;

    struct TrustedUsers {
        list: Vec<UserMask>,
    }

    impl core::fmt::Debug for TrustedUsers {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_list().entries(self.iter()).finish()
        }
    }

    impl TrustedUsers {
        fn from_config(config: &Config) -> Self {
            let list = match config.get_option("default_trust") {
                Some(prefix) => match prefix.parse() {
                    Ok(prefix) => vec![prefix],
                    Err(_) => {
                        error!("warning: default_trust cannot be parsed!");
                        vec![]
                    }
                },
                None => {
                    error!("warning: no default_trust option specified");
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
        known_nickname: RwLock<Option<String>>,
        known_hostmask: RwLock<Option<UserMask>>,
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
                known_nickname: RwLock::default(),
                known_hostmask: RwLock::default(),
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
                        error!("invalid prefix: {:?}", cmd.args());
                    }
                }
                CommandName::Plain(x) if x == "untrust" => {
                    if !check_trust(&slf, source).await {
                        return;
                    }
                    let mut trusted = slf.trusted.write().await;
                    *trusted = TrustedUsers::from_config(&slf.config);
                    error!("trusted list reset");
                }
                CommandName::Plain(x) if x == "trust-list" => {
                    if !check_trust(&slf, source).await {
                        return;
                    }
                    let trusted = slf.trusted.read().await;
                    info!(?trusted, "trust-list");
                }
                CommandName::Plain(x) if x == "load" => {
                    if !check_trust(&slf, source).await {
                        return;
                    }
                    let module_name = cmd.args.trim().to_string();
                    let state = slf.clone();
                    tokio::spawn(async move {
                        let load_result = if module_name.trim().starts_with("https://") {
                            state.rustico().load_module_from_url(&module_name).await
                        } else {
                            state.rustico().load_module(module_name.clone()).await
                        };
                        let response = match load_result {
                            Ok(name) => format!("loaded module: {name}"),
                            Err(error) => {
                                error!(err = %error, module_name, "cannot load module");
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
                    error!(cmd = cmd.as_value(), "not a valid management command");
                }
            }
        }

        fn estimate_overhead(&self, command: &[u8], target: &str) -> usize {
            // very conservative default if we don't know for sure yet
            const DEFAULT_IF_UNKNOWN: usize = 128;
            let prefix_length = match self.known_hostmask.try_read().as_deref() {
                Ok(Some(mask)) => mask.prefix_length(),
                _ => DEFAULT_IF_UNKNOWN,
            };
            // overhead must be calculated considering the relayed message,
            // which contains our own prefix, not the send command:
            // :nick!user@host PRIVMSG target :payload\r\n
            // 1              2       3      45       6 7
            prefix_length + target.bytes().len() + command.len() + 7
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
                let overhead = self.estimate_overhead(b"PRIVMSG", target);
                let max_payload_size = MAX_SIZE.saturating_sub(overhead);
                let boundary = line.floor_char_boundary(max_payload_size);
                let fitted = if boundary < line.len() {
                    // truncate to make it fit, and replace the last bit with a
                    // suffix to mark that truncation happened
                    let truncate_suffix = '…';
                    let boundary =
                        line.floor_char_boundary(max_payload_size - truncate_suffix.len_utf8());
                    let truncated = &line[..boundary];
                    format!("{truncated}{truncate_suffix}")
                } else {
                    line.to_string()
                };
                // let truncated = &line[..boundary];
                let estimated_size = overhead + fitted.bytes().len();
                trace!(
                    target,
                    line = fitted,
                    overhead,
                    estimated_size,
                    "want to send"
                );
                self.throttler.acquire_one().await;
                trace!(target, line = fitted, "enqueued");
                let _ = self.client(|client| client.send_privmsg(target, fitted));
            }
        }

        pub(crate) async fn engine_permit(&self) -> Result<impl Drop + '_, AcquireError> {
            self.engine_semaphore.acquire().await
        }

        pub(crate) fn engine_epoch_timer(self: &Arc<Self>) -> impl core::future::Future {
            let weak = Arc::downgrade(&self.clone());
            struct ServiceRef(Arc<BotState>);
            impl AsRef<rustico::Service> for ServiceRef {
                fn as_ref(&self) -> &rustico::Service {
                    &self.0.rustico
                }
            }
            rustico::Service::epoch_timer(move || weak.upgrade().map(ServiceRef))
        }

        pub(crate) async fn irc_task(self: Arc<Self>) -> Result<(), irc::error::Error> {
            while !self.quitting.load(std::sync::atomic::Ordering::SeqCst) {
                info!("starting new client...");
                let mut client = Client::from_config(self.config.clone()).await?;
                client.identify()?;
                {
                    *self.known_nickname.write().await = None;
                }
                let stream = client.stream()?;
                *self.client.write().await = Some(client);
                match super::irc_stream_handler(stream, self.clone()).await {
                    Ok(_) => {}
                    Err(error) => {
                        error!(err = %error, "irc stream loop terminated");
                    }
                }
            }
            Ok(())
        }

        pub(crate) fn request_quit(&self) {
            let already_quitting = self
                .quitting
                .swap(true, std::sync::atomic::Ordering::SeqCst);
            if !already_quitting {
                let _ = self.client(|client| client.send_quit("requested"));
            }
        }

        pub(super) fn set_last_known_nickname(&self, nickname: String) {
            if let Ok(mut known_nickname) = self.known_nickname.try_write() {
                let previous_nickname = known_nickname.replace(nickname);
                if previous_nickname != *known_nickname {
                    info!(
                        nickname = &*known_nickname,
                        previous_nickname, "changed nickname"
                    );
                    self.client(|client| {
                        self.discover_hostmask(
                            client,
                            known_nickname.as_deref().unwrap_or_default().to_string(),
                        )
                    });
                }
            } else {
                error!(
                    "attempt to acquire rwlock on known_nickname failed; this should not happen"
                );
            }
        }

        fn discover_hostmask(&self, client: &Client, nickname: String) {
            let _ = client.send(irc::proto::Command::WHOIS(None, nickname));
        }

        pub(crate) fn found_hostmask(&self, nick: &str, user: &str, host: &str) {
            let usermask = UserMask::from_parts(nick, user, host);
            if let Ok(mut known_hostmask) = self.known_hostmask.try_write() {
                *known_hostmask = Some(usermask);
            } else {
                error!(
                    "attempt to acquire rwlock on known_hostmask failed; this should not happen"
                );
            }
        }
    }

    impl core::fmt::Debug for BotState {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("BotState")
                .field("quitting", &self.quitting)
                .finish()
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
        info!(message = %message.to_string().trim_end(), "irc message");
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
                    info!(cmd = cmd.as_value(), "got command");
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
            Command::Response(response, args) if !args.is_empty() => {
                // let's extract our last known nickname (some servers might be
                // non-compliant and not include a target as the first argument
                // to the response, but I think it's rare enough to ignore for
                // the moment)
                let target = &args[0];
                let prefix = irc::proto::Prefix::new_from_str(target);
                if let irc::proto::Prefix::Nickname(nickname, _, _) = prefix {
                    state.set_last_known_nickname(nickname);
                } else {
                    warn!(
                        ?response,
                        invalid_target = target,
                        "non-compliant server did not send a valid target in response"
                    );
                }
                // handle specific responses
                #[allow(clippy::single_match)]
                match response {
                    Response::RPL_WHOISUSER => {
                        // "<client> <nick> <username> <host> * :<realname>"
                        if let [client, nick, username, host, _, _realname] = &args[..] {
                            if client == nick {
                                state.found_hostmask(nick, username, host);
                            }
                        } else {
                            warn!("invalid RPL_WHOISUSER response from server");
                        }
                    }
                    _ => {}
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
    run_task
        .spawn(async move {
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
                    error!(error = %err, cmd = cmd.as_value(), "error on command");
                }
            }
            // being super-explicit that engine permit is released only after the
            // whole response has been sent out:
            drop(permit);
        })
        .unwrap();
}

struct ParseError;

#[derive(Debug, Clone, Valuable)]
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

#[derive(Debug, Clone, Valuable)]
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
                        Ok(_) => info!(module, "loaded module"),
                        Err(err) => error!(module, %err, "cannot load module"),
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
                        Some(Ok(_)) => info!(channel = chan_name, "joined channel"),
                        Some(Err(err)) => error!(channel = chan_name, %err, "cannot join channel"),
                        None => {}
                    }
                }
            }
        })
        .map(|_| "");

    #[allow(clippy::let_with_type_underscore)]
    let filter: _ = hello.or(load_module).or(join_channel);

    warp::serve(filter).run(([127, 0, 0, 1], 3030)).await;
}
