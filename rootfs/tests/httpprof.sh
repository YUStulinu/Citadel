# Serve for a while under load from the host, then print the profile and network counters.
/bin/httpd -r /www -q &
sleep ${1:-16}
cat /proc/net
cat /proc/profile
