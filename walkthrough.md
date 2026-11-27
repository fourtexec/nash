# Security Summary

A full security and performance pass was completed across the `nash` Rust codebase (`src/*.rs`) and installer scripts (`install.sh`, `install-dev.sh`).

### Key Vulnerabilities Resolved

1. **Command injection during privileged shell registration (`src/install.rs`)**
   - Previously, `nash install` passed the install path into `sudo sh -c "echo '{}' >> /etc/shells"`. A prefix containing quotes, command substitutions, or newlines could execute arbitrary commands as root or inject extra entries into `/etc/shells`.
   - Fix: Rejected install paths containing control characters or newlines (`is_safe_shell_path`), removed `sudo sh -c`, and replaced it with `sudo tee -a /etc/shells` fed directly over stdin. Also removed shell-spawning `sh -c` lookups in `src/install.rs`, `src/update.rs`, and `src/colors.rs` in favor of direct `PATH` lookups (`sys::find_in_path`) and direct process spawning.

2. **Supply-chain and download integrity weaknesses (`src/update.rs`, `install.sh`, `install-dev.sh`)**
   - Previously, `nash update` silently skipped SHA-256 verification if no external checksum utility was installed, accepted arbitrary URLs via `NASH_UPDATE_URL`, and did not enforce HTTPS/TLS flags or `--` option termination for `curl`/`wget`. The shell installers (`install.sh`, `install-dev.sh`) also downloaded and installed binaries without verifying `SHA256SUMS` and wrote to predictable temporary paths.
   - Fix: Implemented a zero-dependency pure-Rust streaming SHA-256 verifier in `src/update.rs` so checksum verification never fails open. Enforced strict `https://` URL validation, `--proto =https --tlsv1.2 --` for `curl`, and `--` argument termination for `wget`. Updated `install.sh` and `install-dev.sh` to use `mktemp -d` with cleanup traps, strict HTTPS download flags, numeric tag validation in `install-dev.sh`, and mandatory `SHA256SUMS` verification before installation.

3. **Predictable temporary files, unsafe permissions, and symlink races (`src/sys.rs`, `src/install.rs`, `src/update.rs`, `src/history.rs`, `src/builtins.rs`, `src/exec.rs`)**
   - Previously, installers, updaters, history compaction, `z` database saves, last-dir saves, and heredoc files used predictable PID-based temporary filenames or left sensitive files with default umask permissions, and `set -C` (`noclobber`) performed a check-then-open sequence vulnerable to race conditions.
   - Fix: Added `sys::random_u64` (backed by `/dev/urandom`), `sys::create_temp_file` (`O_CREAT | O_EXCL` with 64-bit random suffixes), `sys::write_atomic_private` (`0o600` exclusive temp file + atomic rename), and race-free `sys::open_noclobber` (`O_CREAT | O_EXCL` followed by non-truncating open and `fstat` regular-file verification). Updated history file creation/writes to enforce `0o600` permissions and seeded `Shell::rng` from `/dev/urandom`.

4. **Catastrophic backtracking in glob matching (`src/glob.rs`)**
   - Previously, recursive wildcard matching in `glob::m` had exponential worst-case runtime on repeated `*` patterns (over 1 second for just 14 stars on a 29-character string, and effectively hanging at 20+ stars).
   - Fix: Replaced recursive star backtracking with an O(|pattern| * |input|) iterative two-pointer matcher for standard globs (with a zero-allocation byte fast path for simple `*`/`?` patterns) and a memoized dynamic-programming matcher for extended globs. Added recursion depth and match-count caps for `**` directory traversal and nested extglobs.

5. **Denial-of-service, overflow, and panic bugs across builtins, expansion, arithmetic, and parsing (`src/builtins.rs`, `src/expand.rs`, `src/arith.rs`, `src/parser.rs`, `src/cond.rs`, `src/sys.rs`, `src/main.rs`)**
   - Fixed out-of-bounds and integer underflow panics in `string split -n 0`, `string split -n`, `string join`, `string match`, `string sub`, and `getopts`.
   - Fixed `i64::MIN` negation/subtraction panics in brace range expansion (`{-9223372036854775808..0}`) and `printf` integer parsing (`printf '%d' -9223372036854775808`).
   - Added caps to brace Cartesian product expansion (`100,000` items, depth `64`), `printf` width/precision (`1,000,000` width, `1,000` float precision) with O(n) padding, arithmetic recursion depth (`128`), parser nesting depth (`96`), and per-command alias expansion (`256`).
   - Fixed `base#digits` integer multiplication overflow in `src/arith.rs` and eliminated silent `u32` truncation on large shifts and exponents.
   - Added negative file-descriptor guards in `src/sys.rs`, signal index bounds checks in `take_pending`, null-pointer checks on `getpwuid`/`getpwnam` fields, `pgid > 0` guards before signaling `-pgid`, `--` separator for `zoxide add`, and bounds checks on `regexec` capture offsets in `src/cond.rs`.
   - Fixed `sys::open` to use `libc::open` so `errno` is always updated accurately for redirection error reporting.

## PoC Verification

All fixes were verified against both the debug build (with Rust integer-overflow assertions enabled) and the optimized release build via unit tests (`cargo test` / `cargo test --release`: 25 unit tests + 3 integration suites passing with zero compiler warnings), `sh -n` syntax validation on `install.sh` and `install-dev.sh`, and direct live PoC runs:

| Verification Case | Before | After |
| --- | --- | --- |
| `nash install --prefix '/tmp/x\n/bin/evil' --no-chsh` | Accepted newline in prefix; passed into `sudo sh -c` during `/etc/shells` registration | Rejected immediately (exit 2: `invalid control characters`) |
| `NASH_UPDATE_URL=http://example.com/releases nash update --list` | Accepted plaintext HTTP URL | Rejected immediately (exit 2: `refusing non-HTTPS URL`) |
| `s="aaa...c"; [[ $s == *a*a*...*a*b ]]` (14 stars / 25 stars) | 1,065.2 ms for 14 stars; hung indefinitely for 25 stars | 2.43 ms for 14 stars; 2.06 ms for 25 stars |
| `string split -n 0 , a,b` (plus missing-arg variants) | Panicked (`subtract with overflow` / index OOB) | Handled cleanly without panic (exit 0 / exit 2) |
| `echo {-9223372036854775808..0}` | Panicked (`negate with overflow`) | Prints literal safely (exit 0) |
| `echo {a,b}{a,b}...` (25 repetitions, 33M combinations) | Unbounded memory allocation / OOM | Bails out cleanly at cap and prints literal (exit 0) |
| `echo $((36#zzzzzzzzzzzzzzzzzzzz))` | Panicked (`multiply with overflow`) | Wraps cleanly (exit 0) |
| `echo $(( (((...1...))) ))` (200 nested parens) | Unbounded stack recursion | Clean error: `expression recursion level exceeded` (exit 1) |
| `( ( ... true ... ) )` (150 nested subshells) | Stack overflow (`SIGSEGV`) | Clean error: `maximum nesting depth exceeded` (exit 2) |
| `printf '%d' -9223372036854775808` | Incorrectly failed with `invalid number` | Prints `-9223372036854775808` accurately (exit 0) |
| `printf '%99999999999999999999s' x` | Unbounded width / memory exhaustion | Safely clamped to 1,000,000 bytes (exit 0) |
| `set -C; echo overwritten > existing_file` vs `> /dev/null` | Vulnerable to check-then-open race; stale errno reporting | Atomically rejects regular file (`cannot overwrite existing file`, exit 1) while permitting `/dev/null` (exit 0) |

## Performance Optimizations

Hot-path allocations and redundant work were removed across the parser, arithmetic engine, word expander, glob matcher, command executor, line editor, and release build profile:

- **Parser (`src/parser.rs`)**: Replaced `Vec<char>` storage with UTF-8 byte-indexed `String`, turning `peek_token` into a zero-allocation slice borrow and eliminating per-token `String` allocations across scripts.
- **Arithmetic (`src/arith.rs`, `src/shell.rs`, `src/expand.rs`)**: Refactored `Ev` to parse directly from `&str` with borrowed lvalues, fast first-byte operator dispatch, zero-copy variable reads (`get_cow`), in-place integer variable updates (`set_i64`), and a fast path in `Shell::arith` that skips double-quote re-parsing when no `$`, `` ` ``, or `\` is present.
- **Word expansion (`src/expand.rs`)**: Added direct-to-string expansion (`expand_parts_str`) bypassing intermediate `Vec<(char, bool)>` field buffers for assignments, double-quoted strings, and redirections; added fast paths for literal/quoted/simple-split words; eliminated per-word `IFS` `String` and `Vec<char>` allocations via `IfsMode`.
- **Globbing (`src/glob.rs`)**: Added zero-allocation byte-level matching for simple `*`/`?` globs, byte-level `has_magic`, and removed intermediate `Vec<usize>` allocation in `trim`.
- **Execution & Build (`src/exec.rs`, `src/ast.rs`, `src/editor.rs`, `Cargo.toml`)**: Added zero-alloc `word_literal_str`, reused DP rows in `edit_distance`, built `execve` env directly in the single-scope fast path, removed a redundant sort in `Editor::edited`, and enabled `opt-level = 3` and `codegen-units = 1` with LTO in `Cargo.toml`.

### Release Micro-Benchmark Comparison (`./target/release/nash`, min of runs)

| Benchmark | Baseline | Optimized | Speedup |
| --- | ---: | ---: | ---: |
| `arith_loop_50k` | 104.32 ms | 42.54 ms | 2.45x |
| `expand_loop_50k` | 83.55 ms | 33.69 ms | 2.48x |
| `word_split_30k` | 61.61 ms | 32.72 ms | 1.88x |
| `glob_match_50k` | 95.79 ms | 29.12 ms | 3.29x |
| `parse_exec_6000_cmds` | 41.99 ms | 10.60 ms | 3.96x |
| `glob_redos_14stars` | 1065.20 ms | 2.43 ms | 438x |
