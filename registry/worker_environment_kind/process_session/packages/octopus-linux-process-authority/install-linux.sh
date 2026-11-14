#!/bin/sh
set -eu

if [ "$(id -u)" -ne 0 ] || [ "$#" -ne 2 ]; then
    printf '%s\n' 'Usage: sudo sh install-linux.sh /absolute/path/to/octopus-linux-process-authority operator-user' >&2
    exit 2
fi

authority_binary=$1
operator_user=$2
case "$authority_binary" in
    /*) ;;
    *) printf '%s\n' 'The service binary path must be absolute.' >&2; exit 2 ;;
esac
test -f "$authority_binary"
test -x "$authority_binary"
case "$operator_user" in
    ''|-*) printf '%s\n' 'The operator must be an ordinary Unix login.' >&2; exit 2 ;;
esac
test "$(id -u -- "$operator_user")" -ne 0
test -x /usr/sbin/apparmor_parser
test -f /etc/apparmor.d/abi/4.0
test "$(cat /sys/module/apparmor/parameters/enabled)" = Y
test -f /sys/fs/cgroup/cgroup.controllers

authority_source_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
if ! getent group octopus-process >/dev/null; then
    groupadd --system octopus-process
fi
usermod -a -G octopus-process "$operator_user"
install -d -o root -g root -m 0755 /usr/libexec/octopus
install -o root -g root -m 0755 "$authority_binary" /usr/libexec/octopus/octopus-linux-process-authority
install -o root -g root -m 0644 "$authority_source_dir/octopus-process-authority.service" /etc/systemd/system/octopus-process-authority.service
systemctl daemon-reload
systemctl enable octopus-process-authority.service
systemctl restart octopus-process-authority.service
printf '%s\n' 'Process authority installed. Start a new login session for the operator user before starting Octopus.'
