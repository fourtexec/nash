fn add(a, b=2) { echo $((a + b)); }
add 3
add 3 4
fn fact(n) { if [ $n -le 1 ]; then echo 1; else echo $((n * $(fact $((n-1))))) ; fi; }
fact 5
