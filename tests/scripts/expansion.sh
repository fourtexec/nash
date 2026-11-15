# quoting & expansion
set -- "a b" c
for x in "$@"; do echo "[$x]"; done
for x in $*; do echo "[$x]"; done
echo "$*" "$#"
IFS=:; v="a:b::c"; for x in $v; do echo "<$x>"; done; unset IFS
e=""; echo [$e] ["$e"] ${e:-dflt} ${e-dflt2} ${u-unset} ${u:+alt} ${v:+alt}
echo "${u:=assigned}" $u
echo 'single $x' "double \$x \"q\" \\ \n" $'ansi\tc\x41'
echo a\ b "c"'d'e
x='*'; echo "$x" | cat
echo $((1+2*3)) $((2**10)) $((7%3)) $((1<2 && 2<3)) $((x=5, x+1)) $((0x10)) $((010)) $((2#101))
n=5; echo $((n++ + ++n)) $n $((n>3?1:0))
echo ${#} ${#x} $?
a=(1 2 3); a+=(4); a[5]=6; echo "${a[@]}" ${#a[@]} "${!a[@]}"
echo "${a[@]:1:2}" "${a[-1]}"
str=abcdef; echo ${str:1} ${str:1:2} ${str: -2} ${str%ef} ${str#ab} ${str/cd/XX} ${str^} ${str^^} 
p=/usr/local/bin/tool.tar.gz; echo ${p##*/} ${p%/*} ${p%%.*} ${p#*.}
unset a; echo "[${a[@]}]" "${#a[@]}"
f() { local l=1; g; echo "f:$l"; }; g() { l=2; }; f; echo "outer:[$l]"
h() { echo $1; shift; echo $#; }; h x y z
echo $(( $(echo 3) + 4 ))
echo "$(echo "nested $(echo deep)")"
echo `echo \`echo bt\``
cat <<-EOF
	tab stripped $str
	EOF
cat <<< "herestring $str"
while read -r a b; do echo "$b $a"; done <<EOF
1 2
3 4
echo {a,b,c}{1,2} {01..03} {a..e..2} {5..1} x{,y}
echo "{a,b}" {a} {} a{b
case "x y" in "x "*) echo m1;; esac
case abc in a|b) echo no;; [a-c]bc) echo m2;; esac
case z in a) echo a;& b) echo b;; z) echo z;& q) echo q;; r) echo r;; esac
[ 1 -lt 2 ] && [ "a" != "b" ] && [ -n "x" ] && [ ! -z "x" ] && echo test-ok
[ -f /etc/passwd -a -d /tmp ] && echo and-ok
[[ abc =~ ^a(b)c$ ]] && echo "re ${BASH_REMATCH[1]}"
[[ -z "" && ( 1 -lt 2 || x == y ) ]] && echo cond-ok
[[ "a b" == "a b" ]] && [[ a < b ]] && echo str-ok
if false; then echo 1; elif true; then echo 2; else echo 3; fi
until true; do echo never; done; echo until-ok
for ((i=0;i<3;i++)); do echo -n $i; done; echo
for i in 1 2 3 4; do [ $i = 2 ] && continue; [ $i = 4 ] && break; echo $i; done
(exit 7); echo $?
{ echo grp; } | cat
! true; echo $?
true | false; echo $? ${PIPESTATUS[@]}
echo out >&2 2>/dev/null
echo err 2>&1 >/dev/null | cat
f2() { return 5; }; f2 || echo "f2=$?"
x=1 eval 'echo ev $x'
VAR=tmp sh -c 'echo $VAR'; echo "[$VAR]"
export EXP=1; sh -c 'echo $EXP'
