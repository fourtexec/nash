<p align="center">
  <img src="assets/nash-logo-text.jpg" alt="nash wordmark logo" width="480" />
</p>

# nash

<img src="assets/nash-logo.jpg" alt="nash 1:1 logo" width="96" align="right" />

Install `nashed` (the package manager for `nash`) via Cargo, then install `nash`:
```sh
cargo install nashed
nashed install
```

Install the latest development build or a specific version:
```sh
nashed install dev
nashed install 1.0.1
```

**nash** is a modern descendant of `sh`, written in Rust. It runs your existing
sh/bash scripts, takes the best interactive ideas from zsh, and adds a few of
its own.

```
~/code/nash (main) ❯ fn greet(name, greeting=Hello) { echo "$greeting, $name!"; }
~/code/nash (main) ❯ greet world
Hello, world!
~/code/nash (main) ❯ gti status
nash: gti: command not found (did you mean `git'?)
```

## Build

```bash
cargo build --release --workspace
```

```bash
./target/release/nash
./target/release/nashed --help
```

Run the tests with `cargo test --workspace`.

## Managing nash with nashed

All installation, version switching, updates, and uninstallation are handled by
`nashed` (located in `nashed/` and publishable to crates.io via `cargo publish -p nashed`).

```bash
# Install latest stable nash (sets up binary, ~/.bashrc logo hook, /etc/shells, chsh)
nashed install

# Install latest dev build or an exact version
nashed install dev
nashed install 1.0.1
nashed install dev7

# Install a local nash binary
nashed install ./target/release/nash
```

Options for `nashed install`:
- `--prefix DIR` installs into `DIR/bin`
- `--no-chsh` skips `/etc/shells` and `chsh`
- `--no-logo` skips the `~/.bashrc` startup logo hook
- `--force` reinstalls even if already installed

### Updating and uninstalling

```bash
nashed update             # update nash to the newest stable release
nashed update dev         # update nash to the newest development build
nashed update 1.0.1       # switch nash to a specific release
nashed update --check     # check whether an update is available (exit 1 if so)
nashed update self        # update nashed itself
nashed list               # list releases available for this platform
nashed uninstall          # remove nash, managed rc blocks, and /etc/shells entry
```

Releases are built by `.github/workflows/release.yml`: every push to `main` is a
development build tagged `dev1`, `dev2`, ... (marked as a pre-release), and
pushing a tag `v1.2.3` publishes the stable release `1.2.3`. Every release
carries platform binaries for both `nash` (`nash-<os>-<cpu>`) and `nashed`
(`nashed-<os>-<cpu>`) alongside `SHA256SUMS`.

The same workflow then updates the nash page of
[fourtexec.site](https://fourtexec.site) with the newest releases and changes.
That step needs the `SITE_TOKEN` secret (a fine-grained token with "Contents:
read and write" on `fourtexec/fourtexec.github.io`); without it the site notices
the release by itself within half an hour.

## What you get

### From sh — the whole language

Pipelines, lists (`&&`, `||`, `;`, `&`), all redirections (`<`, `>`, `>>`, `>|`,
`<>`, `n>&m`, here-docs, `<<-`), `if`/`case`/`for`/`while`/`until`, functions,
subshells and groups, parameter expansion (`${x:-d}`, `${x#pat}`, `${x%%pat}` …),
command substitution, arithmetic, field splitting, globbing, `trap`, `getopts`,
`set -e/-u/-x/-f/-C`, job control (`jobs`, `fg`, `bg`, `wait`, `kill`, Ctrl-Z).

### From bash

- Indexed and associative arrays: `a=(1 2)`, `declare -A m=([k]=v)`, `${!m[@]}`, `${a[@]:1:2}`
- `[[ … ]]` with patterns and `=~` (`BASH_REMATCH`), `(( … ))`, C-style `for ((;;))`
- `${x/pat/rep}`, `${x:off:len}`, `${x^^}`, `${x,,}`, `${!ref}`, `${x@Q}`
- Brace expansion `{a,b}`, `{1..10..2}`, `$'…'`, here-strings `<<<`, `&>`, `|&`
- Process substitution `<(cmd)` / `>(cmd)`
- `local`, `declare`, `printf -v`, `read -r -p -a -n -s -t -d`, `mapfile`, `pushd`/`popd`
- `PIPESTATUS`, `pipefail`, `RANDOM`, `SECONDS`, `LINENO`, `PROMPT_COMMAND`
- `!!` and `!$` history expansion, `PS1` escapes (`\u \h \w \W \$ \t …`)

### From zsh

- Recursive globbing `**/*.rs` and extended globs `!(x)`, `@(a|b)`, `+(…)` — always on
- The last stage of a pipeline runs in the shell: `echo hi | read x` just works
- Floating point arithmetic: `$((10 / 4.0))`, `sqrt()`, `min()`, `max()`, `round()` …
- `autocd`, `RPROMPT`, `precmd` / `preexec` / `chpwd` hook functions
- Menu completion: Tab completes, Tab again opens a menu, Tab/arrows cycle through it
- Up/Down search history by the prefix you have already typed
- Aliases work in scripts too; `setopt` / `unsetopt`

### nash's own

| Feature | Example |
| --- | --- |
| Functions with named parameters, defaults and rest args | `fn deploy(env, tag=latest, ...hosts) { … }` |
| Structured error handling | `try { make; make test; } catch { echo "failed: $ERROR"; }` |
| `defer`: run a command when the function returns | `tmp=$(mktemp); defer rm -f "$tmp"` |
| Strict mode in one word | `set -o strict` (errexit + nounset + pipefail) |
| Frecency directory jumping, built in | `z proj` |
| cd without cd | `..`, `...`, `src/` |
| "Did you mean" for mistyped commands | `gti` → `git` |
| History with context | `history --here`, `history --failed`, `history --long` |
| Batteries-included editor | syntax highlighting, autosuggestions, git-aware prompt |
| Picks up where you left off | a new session starts in the directory the previous one was in |
| File-type colours out of the box | `ls`, `la`, `ll`, `grep`, `diff` and the completion menu |

**Named parameters.** Arguments are bound to local variables and checked:

```sh
fn backup(src, dest=/var/backups, ...flags) {
    rsync -a "${flags[@]}" "$src" "$dest"
}
backup            # nash: backup: missing argument `src'
                  # usage: backup <src> [dest] [flags...]
```

**try / catch.** Inside `try`, the first failing command stops the block (like
`set -e`, but scoped and without its surprises). The status is in `$ERROR`.

```sh
try {
    cd "$dir"
    git pull
    cargo build
} catch {
    echo "update failed with status $ERROR" >&2
}
```

**defer.** Deferred commands run in reverse order when the function exits —
however it exits. At top level they run when the shell exits.

**File-type colours.** Interactive shells get a default `LS_COLORS` that
distinguishes directories, symlinks, executables, archives, images, video,
audio, documents, source code, scripts, config files and temporary files.
`ls`, `la`, `ll`, `lla`, `l`, `dir`, `grep`, `diff` and `tree` are aliased to
their colour-enabled forms, and the Tab completion menu uses the same colours.
All of this happens before `~/.nashrc` is read, so you can override it there
(`export LS_COLORS=…`, `unalias ls`); your own `LS_COLORS` is always respected,
and setting `NO_COLOR` turns the feature off.

**Session directory.** nash remembers the directory you were last in and
starts the next interactive session there. This only happens when the shell
starts in your home directory, so a terminal deliberately opened in a project
folder stays there. Put `NASH_RESTORE_DIR=0` in `~/.nashrc` to turn it off.

**History with context.** Every entry records the directory, time, duration
and exit status. Autosuggestions prefer commands you ran in the *current*
directory, so the grey text after your cursor is usually what you want —
press → or End to accept it, Alt-F to accept one word.

**The prompt.** `PS1` is a normal shell variable. Its default is
`\U\C\G\J\M\Q\Z`: `\U` SSH `user@host`, `\C` styled cwd, `\G` git branch,
`\J` running jobs, `\M` duration of slow commands, `\Q` failed exit status,
`\Z` the colored symbol. Edit `PS1` to reorder or drop segments, or mix in bash
escapes (`\u \h \w \W \$ \t …`) plus `\g` (plain git branch), `\?`, `\P`
(pipeline statuses, e.g. `1|0`) and `\L` (duration). `RPROMPT` sets the
right-hand side. The prompt is compiled once and recompiled only when the
string changes; git lookups are cached per directory, so it stays cheap even in
deep trees. `unset PS1` restores the default; `PS1=''` gives an empty prompt.

## Line editor keys

| Key | Action |
| --- | --- |
| Tab / Shift-Tab | complete, open and cycle the completion menu |
| → / End / Ctrl-E | accept the autosuggestion |
| Alt-F | accept one word of the autosuggestion |
| ↑ / ↓ | history search by typed prefix (or move within a multi-line command) |
| Ctrl-R | incremental history search |
| Alt-. | insert the last argument of the previous command |
| Alt-Enter | insert a newline |
| Ctrl-A/E/B/F, Alt-B/F, Ctrl-←/→ | movement |
| Ctrl-K/U/W, Alt-D, Alt-Backspace, Ctrl-Y | kill and yank |
| Ctrl-L, Ctrl-C, Ctrl-D, Ctrl-Z | clear, cancel, exit, suspend job |

An unfinished command (open quote, `if` without `fi`, trailing `|` …) simply
continues on the next line; the whole command is edited and recalled as one.

Completion knows commands, files, variables and special parameters
(`$HO<Tab>`, `${PA<Tab>`, `$?<Tab>`), options for `set -o`, git subcommands and
branches, cargo subcommands, and anything registered with
`complete -W "words…" command`. The shell imports its environment once at
startup, so `$TERM`, `$LANG`, `$EDITOR` and friends are available and are passed
on to child processes.

## Files

| Path | Purpose |
| --- | --- |
| `~/.nashrc` | read by interactive shells (see `examples/nashrc`) |
| `~/.profile` | read by login shells (`nash -l`) |
| `~/.local/share/nash/history` | history (`$NASH_HISTFILE` overrides) |
| `~/.local/share/nash/z` | directory database for `z` |
| `~/.local/share/nash/last_dir` | directory of the previous session |

## Differences from bash worth knowing

- Arrays are dense (as in zsh): assigning `a[5]` on a short array pads it with empty strings, and `unset 'a[i]'` shifts later elements down.
- `**` is always recursive and extended globs are always enabled.
- Aliases are expanded in scripts as well as interactively.
- The last pipeline stage runs in the current shell when it is a builtin, function or compound command.
- Strings are UTF-8; bytes that are not valid UTF-8 are replaced when read into variables.
- Not implemented: `select`, `coproc`, `ulimit`, `bind`, programmable completion functions (`complete -F`), `[n]{var}>` redirections.

## Source layout

| File | Contents |
| --- | --- |
| `parser.rs`, `ast.rs` | recursive-descent parser and syntax tree |
| `expand.rs`, `glob.rs`, `arith.rs` | word expansion, patterns, arithmetic |
| `exec.rs` | pipelines, redirections, job control, functions |
| `builtins.rs`, `cond.rs` | builtin commands, `test` and `[[ ]]` |
| `editor.rs`, `highlight.rs`, `prompt.rs`, `history.rs` | the interactive experience |

Made by fourtexec

## License

MIT
