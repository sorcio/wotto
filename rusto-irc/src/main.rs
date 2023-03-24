use std::sync::Arc;

use futures::prelude::*;
use irc::client::prelude::*;
use warp::Filter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = Config::load("rusto.toml")?;

    let rustico = rustico::Service::new();

    let mut client = Client::from_config(config).await?;
    client.identify()?;

    let stream = client.stream()?;

    let state = Arc::new(BotState::new(client, rustico));

    let irc_task = tokio::spawn({
        let state = state.clone();
        async move { irc_stream_handler(stream, state).await.unwrap() }
    });

    let web_task = tokio::spawn({
        let state = state.clone();
        async { web_server(state).await }
    });

    let _ = tokio::join!(web_task, irc_task);

    Ok(())
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

impl TryFrom<&str> for UserMask {
    type Error = ();

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        irc::proto::Prefix::new_from_str(value).try_into()
    }
}

mod state {
    use std::sync::Arc;

    use irc::client::Client;
    use irc::proto::Prefix;
    use tokio::sync::RwLock;

    use crate::{BotCommand, CommandName, UserMask};

    struct TrustedUsers {
        list: Vec<UserMask>,
    }

    impl Default for TrustedUsers {
        fn default() -> Self {
            Self {
                list: vec!["il_ratto!~ratto@Azzurra-78C0E8BF.sorcio.org"
                    .try_into()
                    .unwrap()],
            }
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
    }

    async fn check_trust(state: &BotState, prefix: Option<Prefix>) -> bool {
        state.trusted.read().await.is_trusted_prefix(prefix)
    }

    pub(crate) struct BotState {
        client: Client,
        rustico: rustico::Service,
        trusted: RwLock<TrustedUsers>,
    }

    impl BotState {
        pub(crate) fn new(client: Client, rustico: rustico::Service) -> Self {
            Self {
                client,
                rustico,
                trusted: RwLock::new(TrustedUsers::default()),
            }
        }

        pub(crate) fn client(&self) -> &Client {
            &self.client
        }

        pub(crate) fn rustico(&self) -> &rustico::Service {
            &self.rustico
        }

        pub(crate) async fn management_command(
            slf: Arc<Self>,
            source: Option<Prefix>,
            response_target: String,
            cmd: BotCommand,
        ) {
            match cmd.command() {
                CommandName::Plain(x) if x == "ping" => {
                    let _ = slf.client.send_notice(response_target, "pong");
                }
                CommandName::Plain(x) if x == "join" => {
                    if !check_trust(&slf, source).await {
                        return;
                    }
                    let chans: Vec<_> = cmd.args.split_whitespace().collect();
                    let _ = slf.client.send_join(chans.join(","));
                }
                CommandName::Plain(x) if x == "trust" => {
                    if !check_trust(&slf, source).await {
                        return;
                    }
                    if let Ok(mask) = Prefix::new_from_str(cmd.args()).try_into() {
                        let mut trusted = slf.trusted.write().await;
                        let message = if trusted.add_trust(&mask) {
                            format!("I now trust {}", mask.nick)
                        } else {
                            "I already trust this user".to_string()
                        };
                        let _ = slf.client.send_notice(response_target, message);
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
                        let _ = state.client().send_notice(response_target, response);
                    });
                }
                _ => {
                    eprintln!("not a valid management command: {cmd:?}");
                }
            }
        }
    }
}

use state::BotState;

async fn irc_stream_handler(
    mut stream: irc::client::ClientStream,
    state: Arc<BotState>,
) -> Result<(), Box<dyn std::error::Error>> {
    let client = state.client();
    while let Some(message) = stream.next().await.transpose()? {
        println!("{:?}", message);
        let prefixes = [
            "!",
            &format!("{} ", client.current_nickname()),
            &format!("{}:", client.current_nickname()),
            &format!("{}!", client.current_nickname()),
        ];
        #[allow(clippy::single_match)]
        match message.command {
            Command::PRIVMSG(_, ref text) => {
                if let Ok(cmd) = BotCommand::parse(&prefixes, text) {
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
                                let _ = state.client().send_notice(response_target, response);
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
    let args = cmd.args.to_string();
    let (module_name, entry_point) = match cmd.command {
        CommandName::Plain(_) => {
            tokio::spawn(async move {
                BotState::management_command(state, source, response_target, cmd).await;
            });
            return;
        }
        CommandName::Namespaced(ns, name) => (ns, name),
    };
    tokio::spawn(async move {
        match state
            .rustico()
            .run_module(module_name, entry_point, args)
            .await
        {
            Ok(s) => handler(s).await,
            Err(err) => {
                eprintln!("error on command: {err}")
            }
        }
    });
}

mod command_parser {
    use nom::branch::alt;
    use nom::bytes::complete::tag;
    use nom::character::complete::{alpha1, alphanumeric1, space0};
    use nom::combinator::{map, recognize};
    use nom::multi::many0_count;
    use nom::sequence::{delimited, pair, separated_pair};
    use nom::{Finish, IResult};

    use crate::{BotCommand, CommandName};

    fn identifier(input: &str) -> IResult<&str, &str> {
        recognize(pair(
            alt((alpha1, tag("_"))),
            many0_count(alt((alphanumeric1, tag("_")))),
        ))(input)
    }

    fn command_name(input: &str) -> IResult<&str, CommandName> {
        alt((
            map(
                separated_pair(identifier, tag("."), identifier),
                |(ns, x)| CommandName::Namespaced(ns.to_string(), x.to_string()),
            ),
            map(identifier, |x| CommandName::Plain(x.to_string())),
        ))(input)
    }

    pub(super) fn command(input: &str) -> Result<BotCommand, nom::error::Error<&str>> {
        let prefix = delimited(space0, tag("!"), space0);

        let mut parser = delimited(prefix, command_name, space0);

        let (args, command_name) = parser(input).finish()?;

        Ok(BotCommand {
            args: args.to_string(),
            command: command_name,
        })
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        macro_rules! test_command {
            ($cmd:expr, Err) => {{
                let result = command($cmd);
                assert!(matches!(command($cmd), Err(_)), "{result:?}");
            }};
            ($cmd:expr, Plain, $x:expr, $args:expr) => {{
                let result = command($cmd);
                match result {
                    Ok(BotCommand {
                        args,
                        command: CommandName::Plain(command),
                    }) if command == $x && args == $args => {}
                    _ => panic!("{result:?}"),
                }
            }};
            ($cmd:expr, Namespaced, $ns:expr, $x:expr, $args:expr) => {{
                let result = command($cmd);
                match result {
                    Ok(BotCommand {
                        args,
                        command: CommandName::Namespaced(ns, command),
                    }) if ns == $ns && command == $x && args == $args => {}
                    _ => panic!("{result:?}"),
                }
            }};
        }
        #[test]
        fn parse() {
            test_command!("", Err);
            test_command!("hello", Err);
            test_command!("!", Err);
            test_command!("!abc", Plain, "abc", "");
            test_command!("! abc", Plain, "abc", "");
            test_command!("!  abc", Plain, "abc", "");
            test_command!(" !abc", Plain, "abc", "");
            test_command!(" ! abc", Plain, "abc", "");
            test_command!("!abc hello world", Plain, "abc", "hello world");
            test_command!("!abc   hello world", Plain, "abc", "hello world");
            test_command!(" !abc   hello world", Plain, "abc", "hello world");
            test_command!("!abc.cde", Namespaced, "abc", "cde", "");
            test_command!("! abc.cde", Namespaced, "abc", "cde", "");
            test_command!("!  abc.cde", Namespaced, "abc", "cde", "");
            test_command!(" !abc.cde", Namespaced, "abc", "cde", "");
            test_command!(" ! abc.cde", Namespaced, "abc", "cde", "");
            test_command!(
                "!abc.cde hello world",
                Namespaced,
                "abc",
                "cde",
                "hello world"
            );
            test_command!(
                "!abc.cde   hello world",
                Namespaced,
                "abc",
                "cde",
                "hello world"
            );
            test_command!(
                " !abc.cde   hello world",
                Namespaced,
                "abc",
                "cde",
                "hello world"
            );
        }
    }
}

struct ParseError;

#[derive(Debug, Clone)]
enum CommandName {
    Plain(String),
    Namespaced(String, String),
}

// impl CommandName {
//     fn parse(raw: &str) -> Self {
//         if let Some((ns, name)) = raw.split_once('.') {
//             Self::Namespaced(ns.to_string(), name.to_string())
//         } else {
//             Self::Plain(raw.to_string())
//         }
//     }
// }

#[derive(Debug, Clone)]
pub(crate) struct BotCommand {
    // prefix: String,
    command: CommandName,
    args: String,
}

impl BotCommand {
    fn parse(_prefixes: &[&str], text: &str) -> Result<Self, ParseError> {
        command_parser::command(text).map_err(|_| ParseError)
        // let (_prefix, stripped) = prefixes
        //     .iter()
        //     .find_map(|prefix| {
        //         let i = text.find(prefix)? + prefix.len();
        //         Some((&text[..i], text[i..].trim()))
        //     })
        //     .ok_or(ParseError)?;
        // let (raw_command, raw_args) = stripped.split_once(' ').ok_or(ParseError)?;
        // let command = CommandName::parse(raw_command);
        // let args = raw_args.trim();
        // Ok(Self {
        //     // prefix: prefix.to_string(),
        //     command,
        //     args: args.to_string(),
        // })
    }

    pub(crate) fn command(&self) -> &CommandName {
        &self.command
    }

    pub(crate) fn args(&self) -> &str {
        &self.args
    }
}

async fn web_server(state: Arc<BotState>) {
    // GET /hello/warp => 200 OK with body "Hello, warp!"
    let hello = warp::path!("hello" / String).map(|name| format!("Hello, {}!", name));
    let load_module = warp::path!("load" / String)
        .and(warp::post())
        .then({
            let state = state.clone();
            move |module: String| {
                let state = state.clone();
                async move {
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
                    match state.client().send_join(&chan_name) {
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
