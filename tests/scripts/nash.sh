# features that are nash's own
fn greet(name, greeting=Hello, ...rest) { echo "$greeting, $name! (${#rest[@]} extra)"; }
greet World; greet Bob Hi x y
greet 2>&1; echo "status=$?"
fn two(a, b) { echo "$a-$b"; }; two 1 2 3 2>&1 | head -1
try { echo before; false; echo not-reached; } catch { echo "caught $ERROR"; }
try { true; } catch { echo never; }; echo "ok=$?"
try { echo $((1/0)); } catch { echo "arith caught"; } 2>/dev/null
f() { try { g; } catch { echo "inner failed with $ERROR"; return 7; }; }; g() { (exit 4); echo no; }
f; echo "f=$?"
cleanup() { defer echo "deferred 1"; defer echo "deferred 2"; echo body; return 3; }
cleanup; echo "cleanup=$?"
echo $((1.5 * 2)) $((10 / 4)) $((10 / 4.0)) $((sqrt(2) > 1.41)) $((max(3, 7, 5))) $((round(2.6))) $((2 ** 0.5 < 1.5))
echo hello | read word; echo "word=$word"
printf 'a\nb\nc\n' | while read l; do n=$((n+1)); done; echo "n=$n"
set -o strict; set -o | grep -E '^(errexit|nounset|pipefail)' | tr -s ' '; set +o strict
cd "$(mktemp -d)" && mkdir -p a/b/c && touch a/1.rs a/b/2.rs a/b/c/3.rs && echo **/*.rs
echo a/!(b) a/b/@(2|9).rs
declare -A m=(k1 v1 k2 v2); echo "${!m[@]} ${m[k2]}"
alias hi='echo hi-alias'
hi there
echo "${BASH_VERSION:-none} ${NASH_VERSION:+has-version}"
