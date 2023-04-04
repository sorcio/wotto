# wotto
An IRC bot extended with WebAssembly modules.

## What is this?

The general idea is that the bot will load any code and execute it in the
sandbox. Eventually it aims to strike some balance between being useful (a 
builtin trust system, a useable API, etc) and chaotic (running untrusted code
from any user).

## Current status

Don't use this. At the current stage this is little more than a personal
experiment to play around with:

- WebAssembly
- Async Rust
- IRC bots!
- anything that looks fun or useful or interesting

As such, there is no clear design or direction. But if this is interesting to
you feel free to reach out and discuss ideas!

There is very little documentation because nothing is stabilized yet, and a lot
of core parts are missing.

## Quick start

1. Clone the repo
2. Prepare a configuration file `wotto.toml`
3. Run `cargo run -p wotto`

## Configuration file

This is basically the configuration file for the [`irc`
crate](https://github.com/aatxe/irc#configuring-irc-clients). A minimal conf:

```toml
server = "my.irc.server"
use_tls = true
nickname = "wotto-the-bot"
options.default_trust = "you!user@host"
```

The last line is necessary to have an initial trusted user that will be able to
perform administrative actions. Right now, only trusted users can load modules.

## Loading WebAssembly modules

As a trusted user, you can issue the `!load` command to load a module. The
command accepts either a URL or a local file name.

### Local files

Local files must be located in the `examples/` directory (not a subdirectory)
and can be loaded like this:

```text
<trusted-user> !load foo.wasm
<wotto-the-bot> >loaded module: foo
```

Once loaded, the module is available by the base name (`foo` in the example).
If another module with the same name was already loaded, it will be replaced.

### From the web

When the argument to the `!load` command is a URL, wotto will decide if it
trusts the source, and then load the module:

```text
<trusted-user> !load foo.wasm https://gist.github.com/some_user/some_gist
<wotto-the-bot> >loaded module: some_user/foo
```

At this moment, only [gist](https://gist.github.com) is a trusted source. The
module name in this case will take the form `user/basename` so that gists from
different users won't clash.

## Interacting with modules

Only one kind of interaction is (currently) supported: commands that take an
input and respond with an output. Each command corresponds to a function
exported by the module.

For example, if the local file `foo.wasm` exports a function called `hello`,
any user can invoke it by name (after the module is loaded):

```text
<someone> !foo.hello lucy
<wotto-the-bot> >Hello, lucy!
```

Or, in case of a module loaded from the web:

```text
<someone> !user/foo.hello lucy
<wotto-the-bot> >Hello, lucy!
```

## Implementing WebAssembly modules

Note that this is extremely preliminary and incomplete. The API for modules is
not well defined and there are some heavy limitations.

The `examples/c` directory includes a toy example implemented in C. The
included [documentation](examples/c/README.md) gives some more details.

## The `irc/` subdirectory

The `irc/` subdirectory contains a copy of the tree from the
[irc](https://github.com/aatxe/irc) crate repository. I applied only a minimal
amount of changes. Check the source repository, and the [README](irc/README.md)
and [LICENSE](irc/LICENSE.md) files for more information and licensing details.

## Contributing

As mentioned above, wotto is not following any design and doesn't have a
specific goal. But any kind of contribution or idea is welcome! Open an issue
or a pull request and I will do my best.

Make sure to comply with the [Code of
Conduct](https://github.com/sorcio/.github/blob/main/.github/CODE_OF_CONDUCT.md)
when interacting on any project space.

## License

This repository is available under the terms of the [MIT license](LICENSE),
with the exception of the files under the `irc/` directory, which are part of a
[separate project](#the-irc-subdirectory) distributed under the original terms.
