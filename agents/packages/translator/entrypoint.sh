#!/bin/sh
if [ -f /etc/resolv.conf.vm ]; then
    cp /etc/resolv.conf.vm /etc/resolv.conf 2>/dev/null || true
fi
if [ -f /etc/hyphae/env ]; then
    set -a
    . /etc/hyphae/env
    set +a
fi
exec node /app/packages/translator/dist/agent.js
