#!/bin/sh
# Runs the command as PUID:PGID (default 1000:1000), under tini as PID 1 (it
# reaps the converters' children and forwards the stop signal). Started as
# root, it first gives each mounted state directory to that user (the whole
# tree only when its owner differs: a directory Docker just created, or a
# PUID change), then drops every privilege, tini's too. Started as another
# user (`docker run --user`), it runs the command as that user.
set -eu

if [ "$(id -u)" != 0 ]; then
    exec tini -- "$@"
fi

uid="${PUID:-1000}"
gid="${PGID:-1000}"
for directory in /var/lib/wuphf/*; do
    mountpoint -q "$directory" || continue
    if [ "$(stat -c %u:%g "$directory")" != "$uid:$gid" ]; then
        chown -R "$uid:$gid" "$directory"
    fi
done
# Files written here (sessions, the database) are the user's alone.
umask 0027

exec setpriv --reuid="$uid" --regid="$gid" --clear-groups \
    --inh-caps=-all --bounding-set=-all -- tini -- "$@"
