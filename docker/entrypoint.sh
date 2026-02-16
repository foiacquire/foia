#!/bin/sh
set -e

DATA_DIR="${DATA_DIR:-/opt/foia}"
USER="${USER_ID:-1000}"
GROUP="${GROUP_ID:-$USER}"
MIGRATE="${MIGRATE:-false}"

# Default Redis URL for container deployments (expects linked 'redis' service)
export REDIS_URL="${REDIS_URL:-redis://redis:6379}"

# Allow BROWSER_LINK_NAME & BROWSER_PORT since chromium fails with HOST header
if [ -z "$FOIA_BROWSER_URL" ] && [ -n "$BROWSER_LINK_NAME" ]; then
    BROWSER_PORT="${BROWSER_PORT:-9222}"
    BROWSER_HOST=$(nslookup "${BROWSER_LINK_NAME}" | grep Address | cut -f 2 -d \  | tail -n 1)
    export FOIA_BROWSER_URL="ws://${BROWSER_HOST}:${BROWSER_PORT}"
fi

# Require $DATA_DIR to be a volume mount
if ! mountpoint -q "$DATA_DIR" 2>/dev/null; then
    echo "ERROR: $DATA_DIR is not a volume mount. Mount a volume to persist documents:"
    echo "  docker run -v /path/to/data:$DATA_DIR ..."
    exit 1
fi

# Start Tor for non-browser HTTP requests unless direct mode is set
if [ "$FOIA_DIRECT" != "1" ] && [ "$FOIA_DIRECT" != "true" ]; then
    if command -v tor >/dev/null 2>&1; then
        echo "Starting Tor daemon..."
        mkdir -p /tmp/tor
        tor --RunAsDaemon 1 --SocksPort 9050 --DataDirectory /tmp/tor
        sleep 2
        echo "Tor daemon started"
    else
        echo "ERROR: Tor not found and FOIA_DIRECT is not set. Install Tor or set FOIA_DIRECT=1."
        exit 1
    fi
fi

# Run migrations if MIGRATE=true
if [ "$MIGRATE" = "true" ] || [ "$MIGRATE" = "1" ] || [ "$MIGRATE" = "yes" ]; then
    echo "Running database migrations..."
    su-exec "$USER:$GROUP" foia --data "$DATA_DIR" db migrate
fi

exec su-exec "$USER:$GROUP" foia --data "$DATA_DIR" "$@"
