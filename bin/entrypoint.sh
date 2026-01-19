#!/bin/sh
# Launch Chromium with remote debugging and socat proxy
# Chromium ignores --remote-debugging-address on Alpine/Debian, so we use socat
# to forward external connections to its localhost-bound port.

set -e

# VNC_PASSWORD enables display on port 5900
# VNC_VIEWONLY=true makes it read-only (default: interactive)
if [ -n "$VNC_PASSWORD" ]; then
    Xvfb :99 -screen 0 1920x1080x24 &
    sleep 1
    export DISPLAY=:99
    VIEWONLY_FLAG=""
    [ "$VNC_VIEWONLY" = "true" ] && VIEWONLY_FLAG="-viewonly"
    x11vnc -display :99 -forever -shared $VIEWONLY_FLAG -passwd "$VNC_PASSWORD" &
    HEADLESS_FLAG=""
else
    HEADLESS_FLAG="--headless"
fi

# Start Chromium on port 9223 (internal)
chromium-browser \
    $HEADLESS_FLAG \
    --no-sandbox \
    --disable-gpu \
    --disable-dev-shm-usage \
    --disable-software-rasterizer \
    --remote-debugging-port=9223 \
    "$@" &
CHROME_PID=$!

# Wait for Chromium to start
sleep 2

# Forward 0.0.0.0:9222 -> 127.0.0.1:9223
socat TCP-LISTEN:9222,fork,reuseaddr,bind=0.0.0.0 TCP:127.0.0.1:9223 &
SOCAT_PID=$!

# Monitor both processes - exit if either dies
while kill -0 $CHROME_PID 2>/dev/null && kill -0 $SOCAT_PID 2>/dev/null; do
    sleep 5
done

echo "Process exited, shutting down..."
kill $CHROME_PID $SOCAT_PID 2>/dev/null || true
exit 1
