#!/usr/bin/env bash
# Build the image locally with podman, ship it to the server over ssh, and
# (re)start only the tunebridge service in the synapse compose stack.
# Nothing is compiled on the server.
#
#   ./deploy.sh                # build + ship + up
#   ./deploy.sh --no-build     # ship the already-built image
#
# Env overrides: HOST (default morgoth), COMPOSE_DIR (default ~/docker/synapse),
# IMAGE (default tunebridge:latest), SUDO (default sudo).
set -euo pipefail

HOST="${HOST:-morgoth}"
COMPOSE_DIR="${COMPOSE_DIR:-\$HOME/docker/synapse}"
IMAGE="${IMAGE:-tunebridge:latest}"
SUDO="${SUDO:-sudo}"
SERVICE="tunebridge"

cd "$(dirname "$0")"

if [[ "${1:-}" != "--no-build" ]]; then
    echo "==> building $IMAGE locally"
    podman build -t "$IMAGE" .
fi

# podman prefixes local images with "localhost/"; docker would keep that name, so
# re-tag under docker.io/library so it loads as plain "$IMAGE".
podman tag "$IMAGE" "docker.io/library/$IMAGE"
echo "==> shipping $IMAGE to $HOST"
podman save --format docker-archive "docker.io/library/$IMAGE" | ssh "$HOST" "$SUDO docker load"

echo "==> starting $SERVICE on $HOST"
ssh "$HOST" "cd $COMPOSE_DIR && $SUDO docker compose up -d --no-deps $SERVICE"

echo "==> status"
ssh "$HOST" "$SUDO docker ps --filter name=$SERVICE --format '{{.Names}}\t{{.Status}}\t{{.Image}}'"
echo "Follow logs with: ssh $HOST $SUDO docker logs -f $SERVICE"
