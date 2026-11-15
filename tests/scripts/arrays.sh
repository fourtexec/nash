a=()
i=0
while [ $i -lt 5 ]; do a+=($i); i=$((i+1)); done
echo "${a[@]}"
echo "len=${#a[@]}"
echo "elem2=${a[2]}"
