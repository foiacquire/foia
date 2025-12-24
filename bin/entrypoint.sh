#!/bin/sh
# Launch Chromium with remote debugging and socat proxy
# Chromium ignores --remote-debugging-address on Alpine/Debian, so we use socat
# to forward external connections to its localhost-bound port.

# Start Chromium on port 9223 (internal)
chromium-browser \
    --headless \
    --no-sandbox \
    --disable-gpu \
    --disable-dev-shm-usage \
    --disable-software-rasterizer \
    --remote-debugging-port=9223 \
    "$@" &

# Wait for Chromium to start
sleep 2

# Forward 0.0.0.0:9222 -> 127.0.0.1:9223
exec socat TCP-LISTEN:9222,fork,reuseaddr,bind=0.0.0.0 TCP:127.0.0.1:9223
