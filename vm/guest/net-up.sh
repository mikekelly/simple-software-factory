#!/bin/bash
# Make and configure the tap gvforwarder attaches to (with -preexisting, so
# it runs no DHCP client): gvproxy on the host is the gateway
# (192.168.127.1) and the DNS server; the guest is always 192.168.127.2.
set -e
ip link show tap0 >/dev/null 2>&1 || ip tuntap add dev tap0 mode tap
ip link set tap0 mtu 1500
ip addr replace 192.168.127.2/24 dev tap0
ip link set tap0 up
ip route replace default via 192.168.127.1 dev tap0
printf 'nameserver 192.168.127.1\n' > /etc/resolv.conf
