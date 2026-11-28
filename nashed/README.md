# nashed

`nashed` is the package and version manager for the [`nash`](https://github.com/fourtexec/nash) shell.

## Install via Cargo

```sh
cargo install nashed
```

This automatically installs the `nashed` command into `$CARGO_HOME/bin` (typically `~/.cargo/bin`).

## Usage

Install the latest stable version of `nash`:

```sh
nashed install
```

Install the latest development build:

```sh
nashed install dev
```

Install a specific version (stable or dev):

```sh
nashed install 1.0.1
nashed install dev7
```

Update `nash` to the latest stable version (or a specific channel/version):

```sh
nashed update
nashed update dev
nashed update 1.0.1
nashed update --check
nashed update --list
```

Update `nashed` itself:

```sh
nashed update self
nashed update self dev
nashed update self --check
```

List available releases or uninstall `nash`:

```sh
nashed list
nashed uninstall
```

### Options

- `--prefix DIR` — install or update `nash` inside `DIR/bin`
- `--no-chsh` — install the binary and `~/.bashrc` logo hook without changing the login shell
- `--no-logo` — do not add the startup logo hook to `~/.bashrc`
- `--check` — check if an update is available (exit status `0`: up to date, `1`: update available)
- `--force` — reinstall even if the target version is already installed
- `--list` — list releases available for this platform

Set `NASH_UPDATE_URL` to override the GitHub API endpoint (HTTPS only).

## License

MIT
