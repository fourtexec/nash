cd "$(mktemp -d)" || exit 1
echo one > f1; echo two >> f1; cat f1
cat < f1 | wc -l | tr -d ' '
ls nonexist 2> err.txt; test -s err.txt && echo has-err
{ echo o; echo e >&2; } > both.txt 2>&1; cat both.txt
{ echo o2; echo e2 >&2; } &> both2.txt; cat both2.txt
exec 3> fd3.txt; echo via3 >&3; exec 3>&-; cat fd3.txt
exec 4< f1; read line <&4; echo "l=$line"; exec 4<&-
set -C; echo x > f1 2>/dev/null || echo noclobber; echo y >| f1; set +C; cat f1
while IFS=: read -r a b c; do echo "$a|$b|$c"; done <<EOF
x:y:z
1:2:3:4
EOF
printf '%s\n' a b c | { read first; echo "first=$first"; cat; }
read -r a b <<< "  lead  trail  "; echo "[$a][$b]"
printf '%d %s %5s|%-5s|%05d %x %o %c %%\n' 42 str r l 42 255 8 char
printf '%s-%s\n' 1 2 3 4
printf '%b' 'a\tb\n'; printf '%q\n' "it's a test"
printf -v pv '%03d' 5; echo $pv
echo -e 'a\nb' ; echo -n no-nl; echo
getopts_test() { OPTIND=1; while getopts "ab:c" o; do case $o in a) echo A;; b) echo "B=$OPTARG";; c) echo C;; ?) echo bad;; esac; done; shift $((OPTIND-1)); echo "rest=$*"; }
getopts_test -a -b val -c x y; getopts_test -ac -bfoo z
trap 'echo exit-trap' EXIT
trap 'echo usr1' USR1; kill -USR1 $$; echo after-usr1
(trap 'echo sub-exit' EXIT; exit 3); echo "sub=$?"
sleep 0.1 & wait $!; echo "wait=$?"
(exit 4) & wait $!; echo "wait=$?"
sleep 0.05 & sleep 0.05 & wait; echo all-done
set -e; false || echo "or-ok"; if false; then :; fi; ! false; echo e-ok; set +e
(set -e; false; echo not-here); echo "sub-e=$?"
set -u; echo "${undefined:-d}"; (echo $undefined) 2>/dev/null; echo "u=$?"; set +u
set -o pipefail; false | true; echo "pf=$?"; set +o pipefail
alias ll='echo aliased'
ll arg
unalias ll
command -v ls >/dev/null && echo cmd-v
x=outer; f() { local x=inner; echo $x; }; f; echo $x
f() { echo "$FUNCNAME $#"; }; f a b
r() { if [ $1 -le 0 ]; then echo 0; else r $(($1-1)); fi; }; r 20
declare -A map; map[a]=1; map[b c]=2; for k in "${!map[@]}"; do echo "$k=${map[$k]}"; done
unset 'map[a]'; echo ${#map[@]}
arr=(one two three); for i in "${!arr[@]}"; do echo "$i:${arr[i]}"; done
echo "${arr[*]}"; IFS=,; echo "${arr[*]}"; unset IFS
arr2=("${arr[@]}" four); echo ${#arr2[@]} ${arr2[@]: -1}
s="a,b,c"; IFS=, read -ra parts <<< "$s"; echo ${#parts[@]} ${parts[1]}
mapfile -t lines < f1; echo ${#lines[@]}
touch a.txt b.txt c.log; echo *.txt; echo *.nomatch; echo [ab].txt; echo ?.log
mkdir -p d1/d2; touch d1/x.rs d1/d2/y.rs; echo **/*.rs
echo "quoted *.txt" '*.txt' \*.txt
x="a b"; set -- $x; echo $#; set -- "$x"; echo $#
echo $(( 5 > 3 )) $(( 5 & 3 )) $(( 5 | 3 )) $(( 5 ^ 3 )) $(( ~5 )) $(( 1 << 4 )) $(( -7 / 2 )) $(( -7 % 3 ))
let "z = 3 * 4"; echo $z; (( z += 1 )); echo $z
eval "echo eval-\$z"
pushd / >/dev/null; pwd; popd >/dev/null
cd /tmp && cd - >/dev/null; echo "st=$?"
echo "a  b" | tr -s ' ' | cut -d' ' -f2
cat <<EOF | sed 's/^/> /'
piped heredoc
EOF
if grep -q two <<< "one two"; then echo grep-ok; fi
select_test=1; [ "$select_test" -eq 1 ] && echo num-eq
test -e f1 -o -e nope && echo test-o
[ "(" = "(" ] && echo paren-eq
[ ! -e nope ] && echo not-e
x=5; [[ $x -gt 3 && $x -lt 10 ]] && echo range
[[ $x == [0-9] ]] && echo digit; [[ abc == *b* ]] && echo contains; [[ abc != *z* ]] && echo ncontains
var="a b c"; for w in $var; do printf '<%s>' "$w"; done; echo
echo ${var// /_} ${var%% *} "${var#* }"
unset var; echo "${var-unset}" "${var:-empty}"
exit 9
