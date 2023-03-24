use nom::branch::alt;
use nom::bytes::complete::tag;
use nom::character::complete::{alpha1, alphanumeric1, hex_digit1, one_of, satisfy, space0};
use nom::combinator::{eof, map, recognize};
use nom::multi::{count, many0, many0_count, many1};
use nom::sequence::{delimited, pair, preceded, separated_pair, terminated, Tuple};
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

fn nickname(input: &str) -> IResult<&str, &str> {
    // Note: this implements only RFC2812-style nicknames. The "modern"
    // standard allows an extended set of characters, but leaves additional
    // restrictions to server implementation. We could lift the
    // restrictions to allow any sequence of UTF-8 characters, but this can
    // introduce security challenges such as homoglyphs and bidirectional
    // text (https://unicode.org/reports/tr36/) that we currently have no
    // defense against. Most servers don't allow that, anyway. Additionally
    // modern IRC allows some characters depending on context: channel type
    // prefixes (like #&) can be used as the non-initial character.
    // Currently, the list of prefixes is not available to the parser so we
    // cannot lift that restriction. Finally, we don't have a restriction
    // on the length of the nickname, because most servers allow longer
    // names, and because message length can already be easily validated
    // if needed. Don't use this parser to parse protocol messages, because
    // it's not safe to do so.

    // nickname   =  ( letter / special ) *8( letter / digit / special / "-" )
    // letter     =  %x41-5A / %x61-7A       ; A-Z / a-z
    // digit      =  %x30-39                 ; 0-9
    // special    =  %x5B-60 / %x7B-7D
    //                  ; "[", "]", "\", "`", "_", "^", "{", "|", "}"
    let letter = |i| one_of("abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ")(i);
    let digit = |i| one_of("0123456789")(i);
    let special = |i| one_of(r"[]\`_^{|}")(i);
    recognize(pair(
        alt((letter, digit)),
        many0(alt((letter, digit, special))),
    ))(input)
}

fn user(input: &str) -> IResult<&str, &str> {
    // Note: for users, we go out of the way and only allow us-ascii
    // printable characters for similar reasons as in nickname(), although
    // the byte sequences matched by RFC2812 would allow UTF-8. We exclude
    // CR and LF although framing should usually do that for us, so this is
    // safer to use in less trusted contexts. Still, no length validation.

    // user       =  1*( %x01-09 / %x0B-0C / %x0E-1F / %x21-3F / %x41-FF )
    //                 ; any octet except NUL, CR, LF, " " and "@"
    recognize(many1(satisfy(|c| c.is_ascii_graphic() && c != '@')))(input)
}

fn host(input: &str) -> IResult<&str, &str> {
    // TODO better validation for hosts

    // host       =  hostname / hostaddr
    // hostname   =  shortname *( "." shortname )
    // shortname  =  ( letter / digit ) *( letter / digit / "-" )
    //               *( letter / digit )
    //                 ; as specified in RFC 1123 [HNAME]
    // hostaddr   =  ip4addr / ip6addr
    // ip4addr    =  1*3digit "." 1*3digit "." 1*3digit "." 1*3digit
    // ip6addr    =  1*hexdigit 7( ":" 1*hexdigit )
    // ip6addr    =/ "0:0:0:0:0:" ( "0" / "FFFF" ) ":" ip4addr

    use nom::character::complete::char;
    use nom::character::complete::u8 as u8_;
    use nom::sequence::tuple;
    let ip4addr = |i| {
        recognize(tuple((
            u8_,
            preceded(char('.'), u8_),
            preceded(char('.'), u8_),
            preceded(char('.'), u8_),
        )))(i)
    };
    let ip6addr = move |i| {
        alt((
            recognize(pair(hex_digit1, count(preceded(tag(":"), hex_digit1), 7))),
            recognize(tuple((
                tag("0:0:0:0:0:"),
                alt((tag("0"), tag("ffff"))),
                ip4addr,
            ))),
        ))(i)
    };
    let hostaddr = move |i| alt((ip4addr, ip6addr))(i);
    let shortname = |i| {
        recognize(pair(
            alphanumeric1,
            many0_count(alt((alphanumeric1, tag("-")))),
        ))(i)
    };
    let hostname = move |i| recognize(pair(shortname, many0(pair(tag("."), shortname))))(i);

    alt((terminated(hostname, eof), terminated(hostaddr, eof)))(input)
}

/// Parse `nick!user@host` style prefixes.
pub(super) fn user_prefix(input: &str) -> Result<(&str, &str, &str), nom::error::Error<&str>> {
    // let (_, ((nick, user), host)) =
    //     (separated_pair(separated_pair(nickname, tag("!"), user), tag("@"), host))(input)
    //         .finish()?;
    use nom::character::complete::char;
    let (_input, (nick, user, host)) = (
        terminated(nickname, char('!')),
        terminated(user, char('@')),
        terminated(host, eof),
    )
        .parse(input)
        .finish()?;
    Ok((nick, user, host))
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
    fn parse_command() {
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

    #[test]
    fn parse_nickname() {
        assert_eq!(nickname("hello"), Ok(("", "hello")));
        assert_eq!(nickname("hello "), Ok((" ", "hello")));
        assert_eq!(nickname("hello!"), Ok(("!", "hello")));
        assert!(matches!(nickname(""), Err(_)));
    }

    #[test]
    fn parse_user() {
        assert_eq!(user("hello"), Ok(("", "hello")));
        assert_eq!(user("hello "), Ok((" ", "hello")));
        assert_eq!(user("hello@"), Ok(("@", "hello")));
        assert!(matches!(user(""), Err(_)));
    }

    #[test]
    fn parse_host() {
        assert_eq!(host("hello"), Ok(("", "hello")));
        assert_eq!(host("example.com"), Ok(("", "example.com")));
        assert_eq!(host("0:0:0:0:0:0:0:0"), Ok(("", "0:0:0:0:0:0:0:0")));
    }

    #[test]
    fn parse_user_prefix() {
        assert_eq!(
            user_prefix("abc!def@example.com"),
            Ok(("abc", "def", "example.com"))
        );
    }
}
