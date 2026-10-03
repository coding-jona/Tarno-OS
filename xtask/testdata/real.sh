# THOS test script run by the real Debian bash: pipes, redirection, command substitution and
# dynamically linked ls / grep / sed together.
echo bash-hello
/usr/bin/ls / | /usr/bin/grep -c bin
echo abc | /usr/bin/sed s/b/X/
n=$(/usr/bin/ls /bin | /usr/bin/grep -c busybox)
echo "busybox-links=$n"
echo real-script-done
